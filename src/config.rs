use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Error, Result};
use regex::Regex;
use toml::{Table, Value};

pub struct Config {
    pub monitors: Vec<MonitorConfig>,
    pub variables: Table,
}

pub struct MonitorConfig {
    pub name: String,

    pub log: Option<PathBuf>,

    pub match_log: Option<Regex>,

    pub exec: Option<String>,
    pub set: Table,
    pub push: Table,
}

pub fn parse(doc: &str) -> Result<Config> {
    let mut table = doc
        .parse::<Table>()
        .map_err(|err| map_to_readable_syntax_err(doc, err))?;
    validate_keys(&table, &["monitor", "notify", "task", "var"])?;

    let variables = match table.remove("var") {
        Some(var) => match var {
            Value::Table(var) => var,
            _ => bail!("Key `var` must be a table."),
        },
        None => Table::new(),
    };

    // Validate and parse monitors.
    let monitor_configs = match table.remove("monitor") {
        Some(monitors) => {
            let monitors = match monitors {
                Value::Table(monitors) => monitors,
                _ => bail!("Key `monitor` must be a table."),
            };

            let mut monitor_configs = Vec::with_capacity(monitors.len());
            for (name, monitor) in monitors {
                let monitor_table = match monitor {
                    Value::Table(monitor) => monitor,
                    _ => bail!("Key `monitor` must be a table."),
                };
                monitor_configs.push(
                    parse_monitor_config(name.clone(), monitor_table)
                        .map_err(|err| anyhow!("Monitor `{name}`: {err}"))?,
                );
            }
            monitor_configs
        }
        None => {
            bail!("No monitors found!");
        }
    };

    Ok(Config {
        monitors: monitor_configs,
        variables,
    })
}

/// Turns a `toml::de::Error` into a human-readable error message.
fn map_to_readable_syntax_err(doc: &str, err: toml::de::Error) -> Error {
    let mut message = err.message().to_owned();
    // Print lines where error occurred.
    if let Some(err_range) = err.span() {
        let mut line_start_byte;
        let mut line_end_byte = 0;
        message += "\n";
        for (i, line) in doc.lines().enumerate() {
            line_start_byte = line_end_byte;
            // Account for new line.
            line_end_byte = line_start_byte + line.len() + 1;
            // Only print the last line.
            if line_end_byte < err_range.end {
                continue;
            }
            message += &format!("\n{}:\t{line}", i + 1);
            message += &format!(
                "\n\t{}{}",
                " ".repeat(err_range.start - line_start_byte),
                "^".repeat(err_range.len())
            );
            break;
        }
    }
    anyhow!("{message}")
}

fn validate_keys(table: &Table, valid_keys: &[&'static str]) -> Result<()> {
    for key in table.keys() {
        if !valid_keys.contains(&key.as_str()) {
            bail!("Invalid key `{key}`");
        }
    }

    Ok(())
}

fn parse_monitor_config(name: String, mut monitor_table: Table) -> Result<MonitorConfig> {
    validate_keys(&monitor_table, &["log", "match_log", "exec", "set", "push"])?;

    let match_log = match monitor_table.remove("match_log") {
        Some(match_log) => {
            let log_regex_str = match_log
                .as_str()
                .ok_or(anyhow!("Key `match_log` must be a string."))?;
            let log_regex = Regex::new(log_regex_str)
                .map_err(|err| anyhow!("Failed to parse match_log: {err}"))?;
            Some(log_regex)
        }
        None => None,
    };

    let log = match monitor_table.get("log") {
        Some(log) => {
            let file_name = log.as_str().ok_or(anyhow!("Key `log` must be a string."))?;
            Some(Path::new(file_name).to_owned())
        }
        None => None,
    };

    let exec = match monitor_table.remove("exec") {
        Some(exec) => {
            // FIXME
            let exec_str = match exec {
                Value::String(exec) => exec,
                _ => bail!("Key `exec` must be a string."),
            };
            Some(exec_str)
        }
        None => None,
    };

    let set = match monitor_table.remove("set") {
        Some(set) => match set {
            Value::Table(set) => set,
            _ => bail!("Key `set` must be a table."),
        },
        None => Table::new(),
    };

    let push = match monitor_table.remove("push") {
        Some(push) => match push {
            Value::Table(push) => push,
            _ => bail!("Key `push` must be a table."),
        },
        None => Table::new(),
    };

    Ok(MonitorConfig {
        name,

        log,

        match_log,

        exec,
        set,
        push,
    })
}
