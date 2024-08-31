use std::{
    io::SeekFrom,
    path::Path,
    process::exit,
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

mod config;

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("ramon=info"))
        .init();

    if let Err(err) = run().await {
        eprintln!("{err}");
        exit(1);
    }
}

async fn run() -> Result<()> {
    let doc = include_str!("../ramon.toml");
    let config = config::parse(doc).map_err(|err| {
        anyhow!(
            "Failed to parse ramon.toml: {err}\n\nRefer to https://github.com/reujab/ramon/wiki"
        )
    })?;

    // TODO: process vars
    // TODO: process notification config
    // TODO: process actions

    // Process monitors.
    for (monitor_name, monitor) in config["monitor"]
        .as_table()
        .expect("The `monitor` key must be a table.")
    {
        info!("Setting up {monitor_name}.");

        // Use multi-line regex in case more than one line is read at a time.
        // FIXME: Multi-line mode does not handle carriage returns. Rewrite for Windows support.
        let log_regex_str = format!("(?m){}", monitor["match_log"].as_str().unwrap());
        let log_regex = Regex::new(&log_regex_str)
            .map_err(|err| anyhow!("Failed to parse match_log for {monitor_name}: {err}"))?;

        let file_name = monitor["log"].as_str().unwrap();
        let mut file = OpenOptions::new()
            .read(true)
            .open(file_name)
            .await
            .map_err(|err| anyhow!("[monitor.{monitor_name}] Failed to open {file_name}: {err}"))?;
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

            info!(
                "[monitor.{monitor_name}] Log file grew by {} bytes",
                size - cursor
            );

            // FIXME: Race condition: What if the file grew after we last checked?
            // Instead of reading until EOF, we should only read `size - cursor` bytes.

            // Ensure chunk ends with newline.
            file.seek(SeekFrom::End(-1)).await?;
            let mut buffer = [0; 1];
            file.read(&mut buffer).await?;
            if buffer[0] != '\n' as u8 {
                warn!("[monitor.{monitor_name}] Log chunk does not end in newline.");
                continue;
            }

            // Match chunk against log_regex and execute on each match.
            file.seek(SeekFrom::Start(cursor)).await?;
            let mut buffer = String::new();
            // FIXME
            file.read_to_string(&mut buffer).await?;
            let trimmed_buffer = &buffer[0..buffer.len() - 1];
            for captures in log_regex.captures_iter(trimmed_buffer) {
                info!("Match found");
                let mut command = Command::new("sh");
                command.args(&["-c", monitor["exec"].as_str().unwrap()]);
                for capture_name in log_regex
                    .capture_names()
                    .filter(Option::is_some)
                    .map(|n| n.unwrap())
                {
                    if let Some(capture) = captures.name(capture_name) {
                        command.env(capture_name, capture.as_str());
                    } else {
                        warn!("[monitor.{monitor_name}] Capture group `{capture_name}` was not found.");
                    }
                }
                command.spawn()?.wait().await?;
            }

            cursor = size;
        }
    }

    Ok(())
}
