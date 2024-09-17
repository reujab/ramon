use std::{collections::HashMap, path::PathBuf, time::Duration};

use anyhow::{anyhow, bail, Error, Result};
use lettre::message::Mailbox;
use regex::Regex;
use tokio::time::{interval, Interval};
use toml::{Table, Value};

pub struct Config {
    pub monitors: Vec<MonitorConfig>,
    pub notifications: HashMap<String, NotificationConfig>,
}

pub struct MonitorConfig {
    pub name: String,

    pub every: Option<Interval>,
    pub log: Option<PathBuf>,
    pub service: Option<String>,

    pub cooldown: Option<Duration>,
    pub match_log: Option<Regex>,
    pub ignore_log: Option<Regex>,
    pub unique: Option<String>,
    pub threshold: Option<(usize, Duration)>,

    pub exec: Option<Exec>,
    pub notify: Option<Notification>,
}

#[derive(Clone, Default)]
pub struct NotificationConfig {
    pub smtp: Option<SmtpConfig>,
}

#[derive(Clone)]
pub struct SmtpConfig {
    pub from: Mailbox,
    pub to: Mailbox,
    pub login: Option<SmtpLogin>,
}

#[derive(Clone)]
pub struct SmtpLogin {
    pub host: String,
    pub username: String,
    pub password: String,
}

pub enum Exec {
    Shell(String),
    Spawn(Vec<String>),
}

pub struct Notification {
    pub r#type: String,
    pub title: String,
    pub body: String,
}

pub fn parse(doc: &str) -> Result<Config> {
    let mut table = doc
        .parse::<Table>()
        .map_err(|err| map_to_readable_syntax_err(doc, err))?;

    let notification_config = match table.remove("notify") {
        None => {
            let mut map = HashMap::new();
            map.insert("default".into(), NotificationConfig::default());
            map
        }
        Some(Value::Table(mut notify)) => {
            let default = match notify.remove("default") {
                None => Table::new(),
                Some(Value::Table(default_table)) => default_table,
                Some(_) => bail!("Key `notify.default` must be a table."),
            };

            let mut hashmap = notify
                .into_iter()
                .map(|(name, config)| Ok((name, parse_notify_config(&default, config)?)))
                .collect::<Result<HashMap<String, NotificationConfig>>>()
                .map_err(|err| anyhow!("Failed to parse notify config: {err}"))?;
            hashmap.insert(
                "default".into(),
                parse_notify_config(&Table::new(), default.into())
                    .map_err(|err| anyhow!("Failed to parse default notification config: {err}"))?,
            );
            hashmap
        }
        Some(_) => bail!("Key `notify` must be a table."),
    };

    // Validate and parse monitors.
    let monitor_configs = match table.remove("monitor") {
        None => bail!("No monitors found!"),
        Some(Value::Table(monitors)) => {
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
        Some(_) => bail!("Key `monitor` must be a table."),
    };

    assert_table_is_empty(table)?;

    Ok(Config {
        monitors: monitor_configs,
        notifications: notification_config,
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

fn parse_notify_config(default: &Table, config: Value) -> Result<NotificationConfig> {
    let mut config_table = match config {
        Value::Table(config_table) => config_table,
        _ => bail!("Key must be a table."),
    };

    for (k, v) in default {
        config_table.entry(k).or_insert(v.to_owned());
    }

    let smtp = match config_table.remove("from") {
        None => None,
        Some(Value::String(from_str)) => {
            let from = from_str
                .parse()
                .map_err(|err| anyhow!("Failed to parse `from`: {err}"))?;

            let to = match config_table.remove("to") {
                None => bail!("Key `to` must be set if `from` is set."),
                Some(Value::String(to_str)) => to_str
                    .parse()
                    .map_err(|err| anyhow!("Failed to parse `to`: {err}"))?,
                Some(_) => bail!("Key `to` must be a string."),
            };

            let login = match config_table.remove("smtp_host") {
                None => None,
                Some(Value::String(host)) => {
                    let username = match config_table.remove("username") {
                        None => {
                            bail!("Key `username` must be set if `smtp_host` is set.")
                        }
                        Some(Value::String(username)) => username,
                        Some(_) => bail!("Key `username` must be a string."),
                    };

                    let password = match config_table.remove("password") {
                        None => {
                            bail!("Key `password` must be set if `smtp_host` is set.")
                        }
                        Some(Value::String(password)) => password,
                        Some(_) => bail!("Key `password` must be a string."),
                    };

                    Some(SmtpLogin {
                        host,
                        username,
                        password,
                    })
                }
                Some(_) => bail!("Key `smtp_host` must be a string."),
            };

            Some(SmtpConfig { from, to, login })
        }
        Some(_) => bail!("Key `from` must be a string."),
    };

    let aggregate = match config_table.remove("aggregate") {
        None => None,
        Some(Value::String(aggregate)) => Some(
            duration_str::parse(aggregate)
                .map_err(|err| anyhow!("Failed to parse `aggregate`: {err}"))?,
        ),
        Some(_) => bail!("Key `aggregate` must be a string."),
    };

    assert_table_is_empty(config_table)?;

    Ok(NotificationConfig { smtp })
}

fn parse_monitor_config(name: String, mut monitor_table: Table) -> Result<MonitorConfig> {
    let every = match monitor_table.remove("every") {
        None => None,
        Some(Value::String(every)) => Some(interval(
            duration_str::parse(every).map_err(|err| anyhow!("Key `every`:\n{err}"))?,
        )),
        Some(_) => bail!("Key `every` must be a string."),
    };

    let log = match monitor_table.remove("log") {
        None => None,
        Some(Value::String(log)) => Some(log.into()),
        Some(_) => bail!("Key `log` must be a string."),
    };

    let service = match monitor_table.remove("service") {
        None => None,
        Some(Value::String(service)) => Some(service),
        Some(_) => bail!("Key `service` must be a string."),
    };

    let cooldown = match monitor_table.remove("cooldown") {
        None => None,
        Some(Value::String(cooldown)) => {
            Some(duration_str::parse(cooldown).map_err(|err| anyhow!("Invalid cooldown:\n{err}"))?)
        }
        Some(_) => bail!("Key `cooldown` must be a string."),
    };

    let match_log = match monitor_table.remove("match_log") {
        None => None,
        Some(Value::String(log_regex_str)) => Some(
            Regex::new(&log_regex_str)
                .map_err(|err| anyhow!("Failed to parse match_log: {err}"))?,
        ),
        Some(_) => bail!("Key `match_log` must be a string."),
    };

    let ignore_log = match monitor_table.remove("ignore_log") {
        None => None,
        Some(Value::String(ignore_log_regex_str)) => Some(
            Regex::new(&ignore_log_regex_str)
                .map_err(|err| anyhow!("Failed to parse ignore_log: {err}"))?,
        ),
        Some(_) => bail!("Key `ignore_log` must be a string."),
    };

    let unique = match monitor_table.remove("unique") {
        None => None,
        Some(Value::String(unique)) => Some(unique),
        Some(_) => bail!("Key `unique` must be a string."),
    };

    let threshold = match monitor_table.remove("threshold") {
        None => None,
        Some(Value::String(threshold)) => {
            let split = threshold.split("/").collect::<Vec<&str>>();
            let (threshold, duration) = match split.len() {
                1 => match &every {
                    None => bail!("Invalid format for threshold: `every` key must be set."),
                    Some(interval) => {
                        let duration = duration_str::parse(split[0])
                            .map_err(|err| anyhow!("Failed to parse threshold duration: {err}"))?;
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
        Some(_) => bail!("Key `threshold` must be a string."),
    };

    let exec = match monitor_table.remove("exec") {
        None => None,
        Some(Value::String(exec_str)) => Some(Exec::Shell(exec_str)),
        Some(Value::Array(args)) => match args.is_empty() {
            true => bail!("Key `exec` must not be empty."),
            false => Some(Exec::Spawn(args.into_iter().map(value_to_string).collect())),
        },
        Some(_) => bail!("Key `exec` must be a string or an array of strings."),
    };

    let notify = match monitor_table.remove("notify") {
        None => None,
        Some(Value::String(title)) => Some(Notification {
            r#type: "default".to_owned(),
            title,
            body: String::new(),
        }),
        Some(Value::Table(mut notification_table)) => Some(Notification {
            r#type: match notification_table.remove("type") {
                None => "default".to_owned(),
                Some(Value::String(t)) => t,
                Some(_) => bail!("Key `type` must be a string."),
            },
            title: match notification_table.remove("title") {
                None => "Ramon Notification".to_owned(),
                Some(Value::String(title)) => title,
                Some(_) => bail!("Key `title` must be a string."),
            },
            body: match notification_table.remove("body") {
                None => String::new(),
                Some(Value::String(body)) => body,
                Some(_) => bail!("Key `body` must be a string."),
            },
        }),
        Some(_) => bail!("Key `notify` must be a string or a table."),
    };

    assert_table_is_empty(monitor_table)?;

    Ok(MonitorConfig {
        name,

        log,
        every,
        service,

        cooldown,
        match_log,
        ignore_log,
        unique,
        threshold,

        exec,
        notify,
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
