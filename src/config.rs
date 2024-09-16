use std::{path::PathBuf, time::Duration};

use anyhow::{anyhow, bail, Error, Result};
use glob::glob;
use regex::Regex;
use tokio::time::{interval, Interval};
use toml::{Table, Value};

pub struct Config {
    pub monitors: Vec<MonitorConfig>,
}

pub struct MonitorConfig {
    pub name: String,

    pub every: Option<Interval>,
    pub log: Option<PathBuf>,
    pub service: Option<String>,
    pub watch: Vec<PathBuf>,

    pub cooldown: Option<Duration>,
    pub match_log: Option<Regex>,
    pub ignore_log: Option<Regex>,
    pub unique: Option<String>,
    pub threshold: Option<(usize, Duration)>,

    pub exec: Option<Exec>,
}

pub enum Exec {
    Shell(String),
    Spawn(Vec<String>),
}

pub fn parse(doc: &str) -> Result<Config> {
    let mut table = doc
        .parse::<Table>()
        .map_err(|err| map_to_readable_syntax_err(doc, err))?;

    // Validate and parse monitors.
    let monitor_configs = match table.remove("monitor") {
        None => bail!("No monitors found!"),
        Some(monitors) => match monitors {
            Value::Table(monitors) => {
                let mut monitor_configs = Vec::with_capacity(monitors.len());
                for (name, monitor) in monitors {
                    let monitor_table = match monitor {
                        Value::Table(monitor) => monitor,
                        _ => bail!("Key `monitor.{name}` must be a table."),
                    };
                    monitor_configs.push(
                        parse_monitor_config(name.clone(), monitor_table)
                            .map_err(|err| anyhow!("Monitor `{name}`: {err}"))?,
                    );
                }
                monitor_configs
            }
            _ => bail!("Key `monitor` must be a table."),
        },
    };

    assert_table_is_empty(table)?;

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

fn parse_monitor_config(name: String, mut monitor_table: Table) -> Result<MonitorConfig> {
    let every = match monitor_table.remove("every") {
        None => None,
        Some(every) => match every {
            Value::String(every_str) => Some(interval(
                duration_str::parse(every_str).map_err(|err| anyhow!("Key `every`:\n{err}"))?,
            )),
            _ => bail!("Key `every` must be a string."),
        },
    };

    let log = match monitor_table.remove("log") {
        None => None,
        Some(log) => match log {
            Value::String(log_str) => Some(log_str.into()),
            _ => bail!("Key `log` must be a string."),
        },
    };

    let service = match monitor_table.remove("service") {
        None => None,
        Some(service) => match service {
            Value::String(service_str) => Some(service_str),
            _ => bail!("Key `service` must be a string."),
        },
    };

    let watch = match monitor_table.remove("watch") {
        None => Vec::new(),
        Some(watch) => {
            let globs = match watch {
                Value::String(glob) => vec![glob],
                Value::Array(globs) => globs
                    .into_iter()
                    .map(|value| match value {
                        Value::String(glob_string) => Ok(glob_string),
                        _ => Err(anyhow!("Key `watch` must be an array of strings.")),
                    })
                    .collect::<Result<Vec<String>, Error>>()?,
                _ => bail!("Key `watch` must be a string or an array of strings."),
            };
            let mut paths = Vec::new();
            for pattern in globs {
                let matches = glob(&pattern)
                    .map_err(|err| anyhow!("Failed to parse glob `{pattern}`: {err}"))?
                    .filter_map(Result::ok);
                paths.extend(matches);
            }
            paths
        }
    };

    let cooldown = match monitor_table.remove("cooldown") {
        None => None,
        Some(cooldown) => match cooldown {
            Value::String(cooldown_str) => Some(
                duration_str::parse(cooldown_str)
                    .map_err(|err| anyhow!("Invalid cooldown:\n{err}"))?,
            ),
            _ => bail!("Key `cooldown` must be a string."),
        },
    };

    let match_log = match monitor_table.remove("match_log") {
        None => None,
        Some(match_log) => {
            let log_regex_str = match_log
                .as_str()
                .ok_or(anyhow!("Key `match_log` must be a string."))?;
            let log_regex = Regex::new(log_regex_str)
                .map_err(|err| anyhow!("Failed to parse match_log: {err}"))?;
            Some(log_regex)
        }
    };

    let ignore_log = match monitor_table.remove("ignore_log") {
        None => None,
        Some(ignore_log) => match ignore_log {
            Value::String(ignore_log_str) => {
                let ignore_regex = Regex::new(&ignore_log_str)
                    .map_err(|err| anyhow!("Failed to parse ignore_log: {err}"))?;
                Some(ignore_regex)
            }
            _ => bail!("Key `ignore_log` must be a string."),
        },
    };

    let unique = match monitor_table.remove("unique") {
        None => None,
        Some(unique) => match unique {
            Value::String(unique) => Some(unique),
            _ => bail!("Key `unique` must be a string."),
        },
    };

    let threshold = match monitor_table.remove("threshold") {
        None => None,
        Some(threshold) => match threshold {
            Value::String(threshold_str) => {
                let split = threshold_str.split("/").collect::<Vec<&str>>();
                let (threshold, duration) = match split.len() {
                    1 => match &every {
                        None => bail!("Invalid format for threshold: `every` key must be set."),
                        Some(interval) => {
                            let duration = duration_str::parse(split[0]).map_err(|err| {
                                anyhow!("Failed to parse threshold duration: {err}")
                            })?;
                            let threshold = duration.as_millis() / interval.period().as_millis();
                            (threshold as usize, duration)
                        }
                    },
                    2 => {
                        let threshold = split[0]
                            .parse()
                            .map_err(|err| anyhow!("Failed to parse threshold: {err}"))?;
                        let duration = duration_str::parse(split[1])
                            .map_err(|err| anyhow!("Failed to parse threshold duration: {err}"))?;
                        (threshold, duration)
                    }
                    _ => bail!("Invalid format for threshold."),
                };
                Some((threshold, duration))
            }
            _ => bail!("Key `threshold` must be a string."),
        },
    };

    let exec = match monitor_table.remove("exec") {
        None => None,
        Some(exec) => match exec {
            Value::String(exec) => Some(Exec::Shell(exec)),
            Value::Array(args) => match args.is_empty() {
                true => bail!("Key `exec` must not be empty."),
                false => Some(Exec::Spawn(args.into_iter().map(value_to_string).collect())),
            },
            _ => bail!("Key `exec` must be a string or an array of strings."),
        },
    };

    assert_table_is_empty(monitor_table)?;

    Ok(MonitorConfig {
        name,

        log,
        every,
        service,
        watch,

        cooldown,
        match_log,
        ignore_log,
        unique,
        threshold,

        exec,
    })
}

pub fn value_to_string(value: Value) -> String {
    match value {
        Value::String(string) => string,
        v => v.to_string(),
    }
}

fn assert_table_is_empty(table: Table) -> Result<()> {
    for key in table.keys() {
        bail!("Invalid key `{key}`");
    }
    Ok(())
}
