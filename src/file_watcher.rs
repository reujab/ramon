use std::path::PathBuf;

use anyhow::{bail, Result};
use log::warn;
use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc::{channel, Sender};

use crate::monitor::Event;

pub async fn watch_files(paths: Vec<PathBuf>, event_tx: Sender<Event>) -> Result<()> {
    let (watcher_tx, mut watcher_rx) = channel(1000);
    let mut watcher = notify::recommended_watcher(move |res| {
        watcher_tx.blocking_send(res).unwrap();
    })?;
    for path in paths {
        if let Err(err) = watcher.watch(&path, RecursiveMode::Recursive) {
            warn!("File watcher error: {err}");
        }
    }
    while let Some(event) = watcher_rx.recv().await {
        let event = match event {
            Err(err) => {
                warn!("File watcher error: {err}");
                continue;
            }
            Ok(event) => event,
        };
        if let EventKind::Access(_) = event.kind {
            continue;
        }
        event_tx.send(Event::FileChange(event.paths)).await?;
    }

    bail!("File watcher exited early.");
}
