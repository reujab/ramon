mod config;
mod log_watcher;
mod monitor;

use anyhow::{anyhow, Result};
use log::error;
use monitor::Monitor;
use std::process::exit;

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
            r#"Failed to parse ramon.toml: {err}

Refer to https://github.com/reujab/ramon#specification-wip"#
        )
    })?;

    // TODO: process notification config

    // Process monitors.
    let mut monitors = Vec::with_capacity(config.monitors.len());
    for monitor_config in config.monitors {
        let name = monitor_config.name.clone();
        let monitor = Monitor::new(monitor_config)
            .await
            .map_err(|err| anyhow!("Monitor `{}`: {err}", name))?;
        monitors.push(monitor);
    }
    let mut handles = Vec::with_capacity(monitors.len());
    for mut monitor in monitors {
        let handle = tokio::spawn(async move {
            let res = monitor.start().await;
            error!("[{}] Monitor exited early.", monitor.name);
            if let Err(err) = &res {
                error!("[{}] {err}", monitor.name);
            }
            res
        });
        handles.push(handle);
    }
    for handle in handles {
        handle.await??;
    }

    Ok(())
}
