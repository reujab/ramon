use std::{
    collections::HashMap,
    io::SeekFrom,
    path::PathBuf,
    sync::Arc,
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
    sync::{
        mpsc::{self, Receiver},
        Mutex,
    },
    time::sleep,
};
use toml::{Table, Value};

use crate::config::MonitorConfig;

pub struct Monitor {
    pub name: String,

    global_variables: Arc<Mutex<HashMap<String, Value>>>,
    local_variables: HashMap<String, Value>,
    set_variables: Table,
    event_rx: Receiver<InternalEvent>,
    watcher: Box<dyn Watcher + Send>,

    log: Option<Log>,
    log_regex: Option<Regex>,

    exec: Option<String>,
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
    _Every,
    NewLogLine(&'a str),
}

impl Monitor {
    pub async fn new(
        config: MonitorConfig,
        global_variables: Arc<Mutex<HashMap<String, Value>>>,
    ) -> Result<Self> {
        let name = config.name;
        let log_file = match config.log {
            Some(path) => {
                let mut file = OpenOptions::new()
                    .read(true)
                    .open(&path)
                    .await
                    .map_err(|err| anyhow!("Failed to open {path:?}: {err}"))?;
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

        Ok(Self {
            name,

            global_variables: global_variables,
            local_variables: HashMap::new(),
            set_variables: config.set,
            event_rx,
            watcher: Box::new(watcher),

            log: log_file,
            log_regex: config.match_log,

            exec: config.exec,
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

        bail!("No more events?");
    }

    async fn process_log_event(&mut self, event: notify::Event) -> Result<()> {
        debug!("[{}] Event: {:?}", self.name, event);

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
            warn!("[{}] File {:?} was truncated", self.name, log.path);
            log.cursor = new_size;
            return Ok(());
        } else if new_size == log.cursor {
            return Ok(());
        }
        self.process_chunk(new_size).await
    }

    async fn reinit_file_descriptors(&mut self) -> Result<()> {
        let prefix = format!("[{}]", self.name);
        let log = self.log.as_mut().unwrap();
        info!(
            "{prefix} File {:?} was renamed. Reestablishing file descriptors.",
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
        info!("{prefix} File descriptors were reestablished.");

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
            self.dispatch_event(ExternalEvent::NewLogLine(line)).await?;
            self.local_variables.clear();
        }

        Ok(())
    }

    async fn dispatch_event<'a>(&mut self, event: ExternalEvent<'a>) -> Result<()> {
        // TODO: cooldown

        if let ExternalEvent::NewLogLine(line) = event {
            if let Some(regex) = &self.log_regex {
                let captures = match regex.captures(line) {
                    Some(captures) => captures,
                    None => return Ok(()),
                };
                debug!("[{}] Match found.", self.name);
                for capture_name in regex
                    .capture_names()
                    .filter(Option::is_some)
                    .map(|n| n.unwrap())
                {
                    if let Some(capture) = captures.name(capture_name) {
                        self.local_variables
                            .insert(capture_name.to_owned(), capture.as_str().into());
                    } else {
                        warn!(
                            "[{}] Capture group `{capture_name}` was not found.",
                            self.name
                        );
                    }
                }
            }

            // TODO: ignore_log
        }

        // TODO: get

        // TODO: if

        // TODO: threshold

        self.run_actions().await
    }

    async fn run_actions(&mut self) -> Result<()> {
        let mut global_vars = self.global_variables.lock().await;
        for (name, value) in self.set_variables.clone() {
            debug!("[{}] Setting {name} = {value}", self.name);
            global_vars.insert(name, value);
        }
        let global_vars_clone = global_vars.clone();
        drop(global_vars);

        for (name, value) in global_vars_clone {
            debug!("Found global variable {} = {}", name, value);
            self.local_variables.entry(name).or_insert(value);
        }

        if let Some(exec) = &self.exec {
            let mut command = Command::new("sh");
            command.args(&["-c", exec]);
            for (var, val) in &self.local_variables {
                command.env(var, var_to_string(val));
            }
            command.spawn()?.wait().await?;
        }

        Ok(())
    }
}

fn var_to_string(value: &Value) -> String {
    match value {
        Value::String(string) => string.to_owned(),
        v => v.to_string(),
    }
}
