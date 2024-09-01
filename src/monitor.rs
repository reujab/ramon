use std::{
    collections::HashMap,
    io::SeekFrom,
    path::{Path, PathBuf},
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
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt},
    process::Command,
    sync::{mpsc, mpsc::Receiver},
    time::sleep,
};
use toml::Table;

pub struct Monitor {
    name: String,
    log_regex: Option<Regex>,
    exec: Option<String>,
    log: Option<Log>,
    watcher: Box<dyn Watcher>,
    event_rx: Receiver<InternalEvent>,
    local_variables: HashMap<String, String>,
}

pub struct Log {
    path: PathBuf,
    file: File,
    cursor: u64,
}

pub enum InternalEvent {
    LogFileEvent(Result<notify::Event, notify::Error>),
}

pub enum ExternalEvent<'a> {
    LogLine(&'a str),
}

impl Monitor {
    pub async fn new(name: String, config: Table) -> Result<Self> {
        let log_regex = match config.get("match_log") {
            Some(match_log) => {
                let log_regex_str = match_log
                    .as_str()
                    .ok_or(anyhow!("[{name}] Key `match_log` must be a string."))?;
                // Use multi-line regex in case more than one line is read at a time.
                // FIXME: Multi-line mode does not handle carriage returns. Rewrite for Windows support.
                let log_regex = Regex::new(log_regex_str)
                    .map_err(|err| anyhow!("[{name}] Failed to parse match_log: {err}"))?;
                Some(log_regex)
            }
            None => None,
        };

        let log_file = match config.get("log") {
            Some(log) => {
                let file_name = log
                    .as_str()
                    .ok_or(anyhow!("[{name}] Key `log` must be a string."))?;
                let path = Path::new(file_name).to_owned();
                let mut file = OpenOptions::new()
                    .read(true)
                    .open(&path)
                    .await
                    .map_err(|err| anyhow!("[{name}] Failed to open {path:?}: {err}"))?;
                file.seek(SeekFrom::End(0)).await?;
                let cursor = file.stream_position().await?;
                Some(Log { path, file, cursor })
            }
            None => None,
        };

        let (event_tx, event_rx) = mpsc::channel(1);
        let mut watcher = notify::recommended_watcher(move |res| {
            event_tx
                .blocking_send(InternalEvent::LogFileEvent(res))
                .unwrap();
        })?;
        if let Some(log_file) = &log_file {
            watcher.watch(&log_file.path, RecursiveMode::NonRecursive)?;
        }

        let exec = match config.get("exec") {
            Some(exec) => {
                // FIXME
                let exec_str = exec
                    .as_str()
                    .ok_or(anyhow!("[{name}] Key `exec` must be a string."))?;
                Some(exec_str.to_owned())
            }
            None => None,
        };

        Ok(Self {
            name,
            log_regex,
            exec,
            log: log_file,
            watcher: Box::new(watcher),
            event_rx,
            local_variables: HashMap::new(),
        })
    }

    pub async fn start(&mut self) -> Result<()> {
        info!("Starting monitor `{}`", self.name);

        while let Some(res) = self.event_rx.recv().await {
            match res {
                InternalEvent::LogFileEvent(event) => match event {
                    Ok(event) => self.process_log_event(event).await?,
                    Err(err) => {
                        error!("[{}] Internal event error: {err}", self.name);
                    }
                },
            };
        }

        bail!("[{}] Monitor exited early.", self.name);
    }

    async fn process_log_event(&mut self, event: notify::Event) -> Result<()> {
        debug!("Event: {:?}", event);

        // Handle move from and deletion. Untested on kernels other than Linux.
        // TODO: Test on other platforms.
        match event.kind {
            EventKind::Modify(ModifyKind::Name(RenameMode::From))
            | EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)) => {
                self.reinit_file_descriptors().await?;
            }
            _ => {}
        }

        let log = self.log.as_mut().unwrap();
        let new_size = log.file.metadata().await?.len();
        if new_size < log.cursor {
            warn!("File {:?} was truncated", log.path);
            log.cursor = new_size;
            return Ok(());
        } else if new_size == log.cursor {
            return Ok(());
        }
        self.process_chunk(new_size).await
    }

    async fn reinit_file_descriptors(&mut self) -> Result<()> {
        let log = self.log.as_mut().unwrap();
        info!(
            "File {:?} was renamed. Reestablishing file descriptors.",
            log.path,
        );

        // Handle log rotation.
        // FIXME: Are there any cases where new log files are not generated immediately
        // after rotation?
        self.watcher.unwatch(&log.path).unwrap();
        let timeout = Instant::now().checked_add(Duration::from_secs(1)).unwrap();
        log.file = loop {
            match OpenOptions::new().read(true).open(&log.path).await {
                Ok(file) => break file,
                Err(err) => {
                    if Instant::now() > timeout {
                        bail!("File {:?} was moved: {err}", log.path);
                    } else {
                        sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        };
        log.cursor = 0;
        self.watcher.watch(&log.path, RecursiveMode::NonRecursive)?;
        info!("File descriptors were reestablished.");

        Ok(())
    }

    async fn process_chunk(&mut self, new_size: u64) -> Result<()> {
        let prefix = format!("[{}]", self.name);
        let log = self.log.as_mut().unwrap();
        let chunk_size = new_size - log.cursor;
        info!("{prefix} Log file grew by {chunk_size} bytes.");
        if chunk_size > 1024 * 1024 {
            warn!("{prefix} Chunk too big. Skipping.");
            log.cursor = new_size;
            return Ok(());
        }

        // Ensure chunk ends with newline.
        // SeekFrom::End is not used here because it introduces a race condition
        // if the file grew immediately after the size was checked.
        log.file.seek(SeekFrom::Start(new_size - 1)).await?;
        let mut buffer = [0; 1];
        log.file.read(&mut buffer).await?;
        if buffer[0] != '\n' as u8 {
            warn!("{prefix} Log chunk does not end in newline.");
            return Ok(());
        }

        log.file.seek(SeekFrom::Start(log.cursor)).await?;
        // Don't read the final newline.
        let mut buffer = vec![0; chunk_size as usize - 1];
        log.file.read_exact(&mut buffer).await?;
        let buffer_str = match String::from_utf8(buffer) {
            Ok(buffer_str) => buffer_str,
            Err(err) => {
                error!("{prefix} Log chunk is not valid UTF-8: {err}");
                log.cursor = new_size;
                return Ok(());
            }
        };
        log.cursor = new_size;
        for line in buffer_str.lines() {
            self.dispatch_event(ExternalEvent::LogLine(line)).await?;
            self.local_variables.clear();
        }

        Ok(())
    }

    async fn dispatch_event<'a>(&mut self, event: ExternalEvent<'a>) -> Result<()> {
        // TODO: cooldown

        if let ExternalEvent::LogLine(line) = event {
            if let Some(regex) = &self.log_regex {
                let captures = match regex.captures(line) {
                    Some(captures) => captures,
                    None => return Ok(()),
                };
                info!("[{}] Match found.", self.name);
                for capture_name in regex
                    .capture_names()
                    .filter(Option::is_some)
                    .map(|n| n.unwrap())
                {
                    if let Some(capture) = captures.name(capture_name) {
                        self.local_variables
                            .insert(capture_name.to_owned(), capture.as_str().to_owned());
                    } else {
                        warn!(
                            "[{}] Capture group `{capture_name}` was not found.",
                            self.name
                        );
                    }
                }
            }
        }

        // TODO: ignore_log

        // TODO: get

        // TODO: if

        // TODO: threshold

        self.run_actions().await
    }

    async fn run_actions(&self) -> Result<()> {
        if let Some(exec) = &self.exec {
            let mut command = Command::new("sh");
            command.args(&["-c", exec]);
            for (var, val) in &self.local_variables {
                command.env(var, val);
            }
            command.spawn()?.wait().await?;
        }
        Ok(())
    }
}
