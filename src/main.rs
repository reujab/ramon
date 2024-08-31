mod config;
mod monitor;

use std::process::exit;

use anyhow::{anyhow, Result};
use monitor::Monitor;

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
        Monitor::new(monitor_name.clone(), monitor.as_table().unwrap().clone())
            .await?
            .start()
            .await?;
    }

    Ok(())
}
