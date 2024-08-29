use std::process::exit;

mod config;

fn main() {
    let doc = include_str!("../radon.toml");
    let config = match config::parse(doc) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("Failed to parse radon.toml: {err}\n\nRefer to https://github.com/reujab/radon/wiki");
            exit(1);
        }
    };
    println!("{config}");

    // TODO: process vars
    // TODO: process notification config

    // Process monitors.
    for (name, monitor) in config["monitor"].as_table().unwrap() {
        println!("{name}: {monitor}");
    }
}
