use crate::monitor::Event;
use anyhow::{anyhow, bail, Result};
use log::{debug, error, info, warn};
use notify::{
    event::{MetadataKind, ModifyKind, RenameMode},
    EventKind, RecursiveMode, Watcher,
};
use std::{
    io::SeekFrom,
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt},
    sync::mpsc::{self, Receiver, Sender},
    time::sleep,
};

pub struct LogWatcher {
    name: String,
    watcher: Box<dyn Watcher + Send>,
    path: PathBuf,
    file: File,
    cursor: u64,
    watcher_rx: Receiver<Result<notify::Event, notify::Error>>,
    event_tx: Sender<Event>,
}

impl LogWatcher {
    pub async fn new(name: String, path: PathBuf, event_tx: Sender<Event>) -> Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .open(&path)
            .await
            .map_err(|err| anyhow!("Failed to open {path:?}: {err}"))?;
        file.seek(SeekFrom::End(0)).await?;
        let cursor = file.stream_position().await?;

        let (watcher_tx, watcher_rx) = mpsc::channel(1);
        let mut watcher = notify::recommended_watcher(move |res| {
            watcher_tx.blocking_send(res).unwrap();
        })?;
        watcher.watch(&path, RecursiveMode::NonRecursive)?;

        Ok(Self {
            name,
            watcher: Box::new(watcher),
            path,
            file,
            cursor,
            watcher_rx,
            event_tx,
        })
    }

    pub async fn start(mut self) -> Result<()> {
        while let Some(res) = self.watcher_rx.recv().await {
            self.process_log_event(res?).await?;
        }
        bail!("No more events.");
    }

    async fn process_log_event(&mut self, event: notify::Event) -> Result<()> {
        debug!("[{}] Event: {event:?}", self.name);

        // Handle move from and deletion. Untested on kernels other than Linux.
        // TODO: Test on other platforms.
        match event.kind {
            EventKind::Modify(ModifyKind::Name(RenameMode::From))
            | EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)) => {
                self.reinit_file_descriptors().await?;
            }
            _ => {}
        }

        let new_size = self.file.metadata().await?.len();
        if new_size < self.cursor {
            warn!("[{}] File {:?} was truncated", self.name, self.path);
            self.cursor = new_size;
            return Ok(());
        } else if new_size == self.cursor {
            return Ok(());
        }
        self.process_chunk(new_size).await
    }

    async fn reinit_file_descriptors(&mut self) -> Result<()> {
        info!(
            "[{}] File {:?} was renamed. Reestablishing file descriptors.",
            self.name, self.path,
        );

        // Handle log rotation.
        // FIXME: Are there any cases where new log files are not generated immediately
        // after rotation?
        self.watcher.unwatch(&self.path).unwrap();
        let timeout = Instant::now().checked_add(Duration::from_secs(1)).unwrap();
        self.file = loop {
            match OpenOptions::new().read(true).open(&self.path).await {
                Ok(file) => break file,
                Err(err) => {
                    if Instant::now() > timeout {
                        bail!("File {:?} was moved: {err}", self.path);
                    } else {
                        sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        };
        self.cursor = 0;
        self.watcher
            .watch(&self.path, RecursiveMode::NonRecursive)?;
        info!("[{}] File descriptors were reestablished.", self.name);

        Ok(())
    }

    async fn process_chunk(&mut self, new_size: u64) -> Result<()> {
        let prefix = format!("[{}]", self.name);
        let chunk_size = new_size - self.cursor;
        info!("{prefix} Log file grew by {chunk_size} bytes.");
        if chunk_size > 1024 * 1024 {
            warn!("{prefix} Chunk too big. Skipping.");
            self.cursor = new_size;
            return Ok(());
        }

        // Ensure chunk ends with newline.
        // SeekFrom::End is not used here because it introduces a race condition
        // if the file grew immediately after the size was checked.
        self.file.seek(SeekFrom::Start(new_size - 1)).await?;
        let mut buffer = [0; 1];
        self.file.read(&mut buffer).await?;
        if buffer[0] != b'\n' {
            warn!("{prefix} Log chunk does not end in newline.");
            return Ok(());
        }

        self.file.seek(SeekFrom::Start(self.cursor)).await?;
        // Don't read the final newline.
        let mut buffer = vec![0; chunk_size as usize - 1];
        self.file.read_exact(&mut buffer).await?;
        let buffer_str = match String::from_utf8(buffer) {
            Ok(buffer_str) => buffer_str,
            Err(err) => {
                error!("{prefix} Log chunk is not valid UTF-8: {err}");
                self.cursor = new_size;
                return Ok(());
            }
        };
        self.cursor = new_size;
        for line in buffer_str.lines() {
            self.event_tx
                .send(Event::NewLogLine(line.to_owned()))
                .await?;
        }

        Ok(())
    }
}
