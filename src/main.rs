use std::process::exit;

use log::info;

mod config;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("ramon=info"))
        .init();

    let doc = include_str!("../ramon.toml");
    let config = match config::parse(doc) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("Failed to parse ramon.toml: {err}\n\nRefer to https://github.com/reujab/ramon/wiki");
            exit(1);
        }
    };
    println!("{config}");

    // TODO: process vars
    // TODO: process notification config

    // Process monitors.
    for (name, monitor) in config["monitor"]
        .as_table()
        .expect("The `monitor` key must be a table.")
    {
        info!("Setting up {name}");
    }
}
