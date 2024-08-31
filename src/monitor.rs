use std::{
    io::SeekFrom,
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Result};
use log::{debug, error, info, warn};
use notify::{
    event::{MetadataKind, ModifyKind, RenameMode},
    EventKind, RecursiveMode, Watcher,
};
use regex::Regex;
use tokio::{
    fs::OpenOptions,
    io::{AsyncReadExt, AsyncSeekExt},
    process::Command,
    sync::mpsc,
    time::sleep,
};
use toml::Table;

pub struct Monitor {
    name: String,
    config: Table,
}

impl Monitor {
    pub fn new(name: String, config: Table) -> Self {
        Self { name, config }
    }

    pub async fn start(&self) -> Result<()> {
        let prefix = format!("[monitor.{}]", self.name);
        info!("Setting up monitor `{}`", self.name);

        // Use multi-line regex in case more than one line is read at a time.
        // FIXME: Multi-line mode does not handle carriage returns. Rewrite for Windows support.
        let log_regex_str = format!("(?m){}", self.config["match_log"].as_str().unwrap());
        let log_regex = Regex::new(&log_regex_str)
            .map_err(|err| anyhow!("Failed to parse match_log for {}: {err}", self.name))?;

        let file_name = self.config["log"].as_str().unwrap();
        let mut file = OpenOptions::new()
            .read(true)
            .open(file_name)
            .await
            .map_err(|err| anyhow!("{prefix} Failed to open {file_name}: {err}"))?;
        file.seek(SeekFrom::End(0)).await?;
        let mut cursor = file.stream_position().await?;

        let (tx, mut rx) = mpsc::channel(1);
        let mut watcher = notify::recommended_watcher(move |res| {
            tx.blocking_send(res).unwrap();
        })?;
        let file_path = Path::new(file_name);
        watcher
            .watch(file_path, RecursiveMode::NonRecursive)
            .unwrap();

        while let Some(res) = rx.recv().await {
            let event = match res {
                Ok(event) => event,
                Err(err) => {
                    error!("Failed monitoring {file_name}: {err}");
                    continue;
                }
            };

            debug!("Event: {:?}", event);
            match event.kind {
                // Handle move from and deletion. Untested on kernels other than Linux.
                // TODO: Test on other platforms.
                EventKind::Modify(ModifyKind::Name(RenameMode::From))
                | EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)) => {
                    info!("File {file_name} was renamed. Reestablishing file descriptors.");

                    // Handle log rotation.
                    // FIXME: Are there any cases where new log files are not generated immediately
                    // after rotation?
                    watcher.unwatch(file_path).unwrap();
                    let timeout = Instant::now().checked_add(Duration::from_secs(1)).unwrap();
                    file = loop {
                        match OpenOptions::new().read(true).open(file_name).await {
                            Ok(file) => break file,
                            Err(err) => {
                                if Instant::now() > timeout {
                                    bail!("File {file_name} was moved: {err}");
                                } else {
                                    sleep(Duration::from_millis(10)).await;
                                }
                            }
                        }
                    };
                    cursor = 0;
                    watcher.watch(file_path, RecursiveMode::NonRecursive)?;
                    info!("File descriptors were reestablished.");
                }
                _ => {}
            }

            let size = file.metadata().await?.len();
            if size < cursor {
                warn!("File {file_name} was truncated");
                cursor = size;
                continue;
            } else if size == cursor {
                continue;
            }
            let chunk_size = size - cursor;

            info!("{prefix} Log file grew by {chunk_size} bytes");

            // Ensure chunk ends with newline.
            // SeekFrom::End is not used because it introduces a race condition if the
            // file grew immediately after the size was checked.
            file.seek(SeekFrom::Start(size - 1)).await?;
            let mut buffer = [0; 1];
            file.read(&mut buffer).await?;
            if buffer[0] != '\n' as u8 {
                warn!("{prefix} Log chunk does not end in newline.");
                continue;
            }

            // Match chunk against log_regex and execute on each match.
            file.seek(SeekFrom::Start(cursor)).await?;
            // Don't read the final newline.
            let mut buffer = vec![0; chunk_size as usize - 1];
            file.read_exact(&mut buffer).await?;
            let buffer_str = match String::from_utf8(buffer) {
                Ok(buffer_str) => buffer_str,
                Err(err) => {
                    error!("{prefix} Log chunk is not valid UTF-8: {err}",);
                    cursor = size;
                    continue;
                }
            };
            for captures in log_regex.captures_iter(&buffer_str) {
                info!("Match found");
                let mut command = Command::new("sh");
                command.args(&["-c", self.config["exec"].as_str().unwrap()]);
                for capture_name in log_regex
                    .capture_names()
                    .filter(Option::is_some)
                    .map(|n| n.unwrap())
                {
                    if let Some(capture) = captures.name(capture_name) {
                        command.env(capture_name, capture.as_str());
                    } else {
                        warn!("{prefix} Capture group `{capture_name}` was not found.");
                    }
                }
                command.spawn()?.wait().await?;
            }

            cursor = size;
        }

        bail!("{prefix} Monitor exited early.");
    }
}
