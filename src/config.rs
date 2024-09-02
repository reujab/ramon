use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Error, Result};
use regex::Regex;
use toml::Table;

pub struct Config {
    pub monitors: Vec<MonitorConfig>,
}

pub struct MonitorConfig {
    pub name: String,
    pub match_log: Option<Regex>,
    pub log: Option<PathBuf>,
    pub exec: Option<String>,
    pub set: Vec<Variable>,
}

#[derive(Clone)]
pub struct Variable {
    pub name: String,
    pub value: String,
}

pub fn parse(doc: &str) -> Result<Config> {
    let table = doc
        .parse::<Table>()
        .map_err(|err| map_to_readable_syntax_err(doc, err))?;
    validate_keys(&table, &["monitor", "notify", "task", "var"])?;

    // // Validate variables.
    // if let Some(vars) = table.get("var").and_then(|vars| vars.as_table()) {
    //     for (var_name, var) in vars {
    //         match *var {
    //             Value::Table(ref var) => validate_keys(var, &["length", "store"])
    //                 .map_err(|err| anyhow!("Invalid variable `{var_name}`: {err}"))?,
    //             _ => {}
    //         }
    //     }
    // }

    // // Validate notification config.
    // if let Some(_notify_config) = table.get("notify").and_then(|notify| notify.as_table()) {
    //     // TODO
    // }

    // Validate and parse monitors.
    let monitor_configs = match table.get("monitor") {
        None => {
            bail!("No monitors found!");
        }
        Some(monitors) => {
            let monitors = monitors
                .as_table()
                .ok_or(anyhow!("Key `monitor` must be a table."))?;

            let mut monitor_configs = Vec::with_capacity(monitors.len());
            for (name, monitor) in monitors {
                let monitor_table = monitor
                    .as_table()
                    .ok_or(anyhow!("Monitor `{name}` must be a table."))?;
                monitor_configs.push(
                    parse_monitor_config(name.clone(), monitor_table)
                        .map_err(|err| anyhow!("Monitor `{name}: {err}"))?,
                );
            }
            monitor_configs
        }
    };

    Ok(Config {
        monitors: monitor_configs,
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
            if line_end_byte < err_range.start {
                continue;
            }
            message += &format!("\n{}:\t{line}", i + 1);
            message += &format!(
                "\n\t{}{}",
                " ".repeat(err_range.start - line_start_byte - 1),
                "^".repeat(err_range.len())
            );
            if line_end_byte >= err_range.end {
                break;
            }
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

fn parse_monitor_config(name: String, monitor_table: &Table) -> Result<MonitorConfig> {
    validate_keys(monitor_table, &["log", "match_log", "exec", "set"])?;

    let match_log = match monitor_table.get("match_log") {
        Some(match_log) => {
            let log_regex_str = match_log
                .as_str()
                .ok_or(anyhow!("Key `match_log` must be a string."))?;
            // Use multi-line regex in case more than one line is read at a time.
            // FIXME: Multi-line mode does not handle carriage returns. Rewrite for Windows support.
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

    let exec = match monitor_table.get("exec") {
        Some(exec) => {
            // FIXME
            let exec_str = exec
                .as_str()
                .ok_or(anyhow!("Key `exec` must be a string."))?;
            Some(exec_str.to_owned())
        }
        None => None,
    };

    let set = match monitor_table.get("set") {
        Some(set) => {
            let set_table = set
                .as_table()
                .ok_or(anyhow!("Key `set` must be a table."))?;

            let mut vars = Vec::with_capacity(set_table.len());
            for (name, value) in set_table {
                vars.push(Variable {
                    name: name.to_owned(),
                    value: value.as_str().unwrap().to_owned(),
                });
            }
            vars
        }
        None => Vec::new(),
    };

    Ok(MonitorConfig {
        name,
        match_log,
        log,
        exec,
        set,
    })
}
