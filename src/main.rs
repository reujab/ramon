mod config;
mod monitor;

use std::process::exit;

use anyhow::{anyhow, Result};
use log::error;
use monitor::Monitor;
use sqlx::SqlitePool;

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

    let pool = SqlitePool::connect(":memory:").await?;
    sqlx::migrate!("./migrations").run(&pool).await?;

    // TODO: process vars
    // TODO: process notification config
    // TODO: process actions

    // Process monitors.
    let mut monitors = Vec::with_capacity(config.monitors.len());
    for monitor_config in config.monitors {
        let monitor = Monitor::new(monitor_config, pool.clone()).await?;
        monitors.push(monitor);
    }
    let mut handles = Vec::with_capacity(monitors.len());
    for mut monitor in monitors {
        let handle = tokio::spawn(async move {
            let res = monitor.start().await;
            if let Err(err) = &res {
                error!("{err}");
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
