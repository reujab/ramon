use crate::{
    config::{value_to_string, Exec, MonitorConfig},
    log_watcher::LogWatcher,
};
use anyhow::{anyhow, bail, Result};
use log::{debug, error, info, warn};
use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    fs::{create_dir, rename, OpenOptions},
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
    process::Command,
    sync::mpsc::{self, Receiver},
};
use toml::Value;

pub struct Monitor {
    pub name: String,

    event_rx: Receiver<Event>,
    last_action_time: Option<Instant>,

    cooldown: Option<Duration>,
    log_regex: Option<Regex>,
    unique: Option<Unique>,

    exec: Option<Exec>,
}

pub enum Event {
    Tick,
    NewLogLine(String),
}

pub struct Unique {
    variable_name: String,
    recorded_values: HashSet<String>,
}

impl Monitor {
    pub async fn new(config: MonitorConfig) -> Result<Self> {
        let name = config.name;

        let (event_tx, event_rx) = mpsc::channel(1);

        if let Some(mut interval) = config.every {
            let tx = event_tx.clone();
            tokio::spawn(async move {
                loop {
                    interval.tick().await;
                    tx.send(Event::Tick).await.unwrap();
                }
            });
        }

        if let Some(log) = config.log {
            let log_watcher = LogWatcher::new(name.clone(), log, event_tx.clone()).await?;
            let name = name.clone();
            tokio::spawn(async move {
                if let Err(err) = log_watcher.start().await {
                    error!("[{name}] Log watcher: {err}");
                }
            });
        }

        if let Some(service) = config.service {
            let child = Command::new("journalctl")
                .args(["-n0", "-fu", &service])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .spawn()
                .map_err(|err| anyhow!("Failed to spawn journalctl: {err}"))?;
            let stdout = child.stdout.ok_or(anyhow!("Failed to capture stdout."))?;
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            let name = name.clone();
            let event_tx = event_tx.clone();
            tokio::spawn(async move {
                while let Some(line) = lines.next_line().await.unwrap() {
                    event_tx.send(Event::NewLogLine(line)).await.unwrap();
                }
                error!("[{name}] Service watcher exited early.");
            });
        }

        let unique = match config.unique {
            None => None,
            Some(variable_name) => {
                let file_path = format!("/var/cache/ramon/unique_{name}");
                let recorded_values = match OpenOptions::new().read(true).open(file_path).await {
                    Err(_) => HashSet::new(),
                    Ok(file) => {
                        let mut values = HashSet::new();
                        let reader = BufReader::new(file);
                        let mut lines = reader.lines();
                        while let Some(line) = lines.next_line().await? {
                            values.insert(line);
                        }
                        values
                    }
                };
                Some(Unique {
                    variable_name,
                    recorded_values,
                })
            }
        };

        Ok(Self {
            name,

            event_rx,
            last_action_time: None,

            cooldown: config.cooldown,
            log_regex: config.match_log,
            unique,

            exec: config.exec,
        })
    }

    pub async fn start(&mut self) -> Result<()> {
        info!("Starting monitor `{}`", self.name);

        while let Some(event) = self.event_rx.recv().await {
            self.evaluate(event).await?;
        }

        bail!("No more events?");
    }

    /// Evaluate all conditions to determine if actions should be run.
    async fn evaluate(&mut self, event: Event) -> Result<()> {
        if let Some(cooldown) = self.cooldown {
            if let Some(last_action_time) = self.last_action_time {
                if Instant::now().duration_since(last_action_time) < cooldown {
                    info!("[{}] Still cooling down.", self.name);
                    return Ok(());
                }
            }
        }

        let mut temp_variables = HashMap::new();

        if let Event::NewLogLine(line) = event {
            if let Some(regex) = &self.log_regex {
                let captures = match regex.captures(&line) {
                    Some(captures) => captures,
                    // No captures; skip line.
                    None => return Ok(()),
                };
                debug!("[{}] Match found.", self.name);
                for capture_name in regex
                    .capture_names()
                    .filter(Option::is_some)
                    .map(|n| n.unwrap())
                {
                    if let Some(capture) = captures.name(capture_name) {
                        temp_variables.insert(capture_name.to_owned(), capture.as_str().into());
                    } else {
                        warn!(
                            "[{}] Capture group `{capture_name}` was not found.",
                            self.name
                        );
                    }
                }
            }
        }

        if let Some(unique) = &mut self.unique {
            if let Some(var) = temp_variables
                .get(&unique.variable_name)
                .and_then(|v: &Value| v.as_str())
            {
                if unique.recorded_values.contains(var) {
                    return Ok(());
                } else {
                    unique.recorded_values.insert(var.to_owned());
                    if let Err(err) = self.store_unique_values().await {
                        warn!("[{}] Failed to store unique values: {err}", self.name);
                    }
                }
            }
        }

        // TODO: get

        // TODO: if

        // TODO: threshold

        self.run_actions(temp_variables).await
    }

    async fn store_unique_values(&mut self) -> Result<()> {
        let _ = create_dir("/var/cache/ramon").await;

        let file_path = format!("/var/cache/ramon/unique_{}", self.name);
        let tmp_file_path = format!("{file_path}.new");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .open(&tmp_file_path)
            .await
            .map_err(|err| anyhow!("Failed to create {tmp_file_path}: {err}"))?;
        let mut writer = BufWriter::new(file);

        let variables = match &self.unique {
            None => panic!(),
            Some(values) => &values.recorded_values,
        };
        for variable in variables {
            writer.write(variable.as_bytes()).await?;
            writer.write_u8(b'\n').await?;
        }
        writer.flush().await?;

        rename(tmp_file_path, file_path).await?;

        Ok(())
    }

    async fn run_actions(&mut self, temp_variables: HashMap<String, Value>) -> Result<()> {
        self.last_action_time = Some(Instant::now());

        if let Some(exec) = &self.exec {
            let mut command = match exec {
                Exec::Shell(sh_command) => {
                    let mut command = Command::new("sh");
                    command.args(["-c", sh_command]);
                    command
                }
                Exec::Spawn(args) => {
                    let mut command = Command::new(&args[0]);
                    command.args(&args[1..]);
                    command
                }
            };
            for (var, val) in &temp_variables {
                command.env(var, value_to_string((*val).clone()));
            }
            let mut child = command.spawn()?;
            tokio::spawn(async move {
                if let Err(err) = child.wait().await {
                    error!("{err}");
                }
            });
        }

        Ok(())
    }
}
