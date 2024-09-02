use crate::{
    config::{value_to_string, Exec, MonitorConfig},
    log_watcher::LogWatcher,
};
use anyhow::{bail, Result};
use log::{debug, error, info, warn};
use regex::Regex;
use std::{collections::HashMap, sync::Arc};
use tokio::{
    process::Command,
    sync::{
        mpsc::{self, Receiver},
        Mutex,
    },
};
use toml::{value::Array, Table, Value};

pub struct Monitor {
    pub name: String,

    global_variables: Arc<Mutex<HashMap<String, Value>>>,
    local_variables: HashMap<String, Value>,
    event_rx: Receiver<Event>,

    log_regex: Option<Regex>,

    set_variables: Table,
    push: Table,
    exec: Option<Exec>,
}

pub enum Event {
    _Every,
    NewLogLine(String),
}

impl Monitor {
    pub async fn new(
        config: MonitorConfig,
        global_variables: Arc<Mutex<HashMap<String, Value>>>,
    ) -> Result<Self> {
        let name = config.name;

        let (event_tx, event_rx) = mpsc::channel(1);

        if let Some(log) = config.log {
            let log_watcher = LogWatcher::new(name.clone(), log, event_tx.clone()).await?;
            let name = name.clone();
            tokio::spawn(async move {
                if let Err(err) = log_watcher.start().await {
                    error!("[{name}] Log watcher: {err}");
                }
            });
        }

        Ok(Self {
            name,

            global_variables,
            local_variables: HashMap::new(),
            event_rx,

            log_regex: config.match_log,

            set_variables: config.set,
            push: config.push,
            exec: config.exec,
        })
    }

    pub async fn start(&mut self) -> Result<()> {
        info!("Starting monitor `{}`", self.name);

        while let Some(event) = self.event_rx.recv().await {
            self.dispatch_event(event).await?;
        }

        bail!("No more events?");
    }

    async fn dispatch_event(&mut self, event: Event) -> Result<()> {
        // TODO: cooldown

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
                        self.local_variables
                            .insert(capture_name.to_owned(), capture.as_str().into());
                    } else {
                        warn!(
                            "[{}] Capture group `{capture_name}` was not found.",
                            self.name
                        );
                    }
                }
            }

            // TODO: ignore_log
        }

        // TODO: get

        // TODO: if

        // TODO: threshold

        self.run_actions().await
    }

    async fn run_actions(&mut self) -> Result<()> {
        self.sync_local_vars().await?;

        if let Some(exec) = &self.exec {
            let mut command = match exec {
                Exec::Shell(sh_command) => {
                    let mut command = Command::new("sh");
                    command.args(&["-c", sh_command]);
                    command
                }
                Exec::Spawn(args) => {
                    let mut command = Command::new(&args[0]);
                    command.args(&args[1..]);
                    command
                }
            };
            for (var, val) in &self.local_variables {
                command.env(var, value_to_string(val.to_owned()));
            }
            let mut child = command.spawn()?;
            tokio::spawn(async move {
                if let Err(err) = child.wait().await {
                    error!("{err}");
                }
            });
        }

        self.local_variables.clear();

        Ok(())
    }

    /// Modify global variables and then copy them locally.
    async fn sync_local_vars(&mut self) -> Result<()> {
        let mut global_vars = self.global_variables.lock().await;

        for (name, value) in self.set_variables.clone() {
            debug!("[{}] Setting {name} = {value}", self.name);
            global_vars.insert(name, value);
        }

        for (array_name, value) in self.push.clone() {
            info!("[{}] Pushing {value} to {array_name}", self.name);
            let cap = global_vars
                .get(&format!("{array_name}_cap"))
                .and_then(|cap| cap.as_integer())
                .filter(|cap| *cap > 0)
                .map(|cap| cap as usize)
                .unwrap_or(usize::MAX);
            match global_vars
                .entry(array_name.clone())
                .or_insert(Array::new().into())
            {
                Value::Array(array) => {
                    // Check here because we don't check on set.
                    if array.len() > cap {
                        error!("[{}] Array {array_name:?} has {} items but is capped at {}. Truncating.", self.name, array.len(), cap);
                        array.truncate(cap);
                    }
                    if array.len() == cap {
                        // It would be more performant to use a rotating index instead,
                        // but I'm not too worried about this micro-optimization.
                        array.rotate_left(1);
                        *array.last_mut().unwrap() = value;
                    } else {
                        array.push(value)
                    }
                }
                _ => bail!("Cannot push to {array_name:?}: Not an array"),
            }
        }

        for (name, value) in &*global_vars {
            debug!("Found global variable {} = {}", name, value);
            self.local_variables
                .entry(name.clone())
                .or_insert_with(|| value.clone());
        }

        Ok(())
    }
}
