use crate::{
    config::{value_to_string, Exec, MonitorConfig},
    log_watcher::LogWatcher,
};
use anyhow::{anyhow, bail, Result};
use log::{debug, error, info, warn};
use regex::Regex;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    process::Command,
    sync::{
        mpsc::{self, Receiver, Sender},
        RwLock,
    },
};
use toml::{value::Array, Table, Value};

pub struct Monitor {
    pub name: String,

    global_variables: Arc<RwLock<HashMap<String, Arc<Value>>>>,
    event_tx_map: Arc<RwLock<HashMap<String, Sender<Event>>>>,
    event_rx: Receiver<Event>,
    last_action_time: Option<Instant>,
    mutates_globals: bool,

    cooldown: Option<Duration>,
    log_regex: Option<Regex>,

    set_variables: Table,
    push: Table,
    exec: Option<Exec>,
    call: Vec<String>,
}

pub enum Event {
    Tick,
    NewLogLine(String),
    Invocation(HashMap<String, Arc<Value>>),
}

impl Monitor {
    pub async fn new(
        config: MonitorConfig,
        global_variables: Arc<RwLock<HashMap<String, Arc<Value>>>>,
        event_tx_map: Arc<RwLock<HashMap<String, Sender<Event>>>>,
    ) -> Result<Self> {
        let name = config.name;

        let (event_tx, event_rx) = mpsc::channel(1);

        event_tx_map
            .write()
            .await
            .insert(name.clone(), event_tx.clone());

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

        Ok(Self {
            name,

            global_variables,
            event_tx_map,
            event_rx,
            last_action_time: None,
            mutates_globals: config.mutates_globals,

            cooldown: config.cooldown,
            log_regex: config.match_log,

            set_variables: config.set,
            push: config.push,
            exec: config.exec,
            call: config.call,
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

        let temp_variables = match event {
            Event::NewLogLine(line) => {
                let mut temp_variables = HashMap::new();

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
                            temp_variables
                                .insert(capture_name.to_owned(), Arc::new(capture.as_str().into()));
                        } else {
                            warn!(
                                "[{}] Capture group `{capture_name}` was not found.",
                                self.name
                            );
                        }
                    }
                }

                // TODO: ignore_log

                temp_variables
            }
            Event::Invocation(vars) => vars,
            _ => HashMap::new(),
        };

        // TODO: get

        // TODO: if

        // TODO: threshold

        self.run_actions(temp_variables).await
    }

    async fn run_actions(&mut self, mut temp_variables: HashMap<String, Arc<Value>>) -> Result<()> {
        self.last_action_time = Some(Instant::now());
        self.sync_variables(&mut temp_variables).await?;

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
            for (var, val) in &temp_variables {
                command.env(var, value_to_string((**val).clone()));
            }
            let mut child = command.spawn()?;
            tokio::spawn(async move {
                if let Err(err) = child.wait().await {
                    error!("{err}");
                }
            });
        }

        for monitor_name in &self.call {
            self.event_tx_map
                .read()
                .await
                .get(monitor_name)
                .ok_or(anyhow!("Monitor not found: {monitor_name}"))?
                .send(Event::Invocation(temp_variables.clone()))
                .await?;
        }

        Ok(())
    }

    /// Modify global variables and then copy them locally.
    async fn sync_variables(
        &mut self,
        temp_variables: &mut HashMap<String, Arc<Value>>,
    ) -> Result<()> {
        if !self.mutates_globals {
            for (name, value) in &*self.global_variables.read().await {
                temp_variables
                    .entry(name.to_owned())
                    .or_insert_with(|| value.clone());
            }
            return Ok(());
        }

        let mut global_vars = self.global_variables.write().await;

        for (name, value) in self.set_variables.clone() {
            debug!("[{}] Setting {name} = {value}", self.name);
            global_vars.insert(name, value.into());
        }

        for (array_name, value) in self.push.clone() {
            info!("[{}] Pushing {value} to {array_name}", self.name);
            let cap = global_vars
                .get(&format!("{array_name}_cap"))
                .and_then(|cap| cap.as_integer())
                .filter(|cap| *cap > 0)
                .map(|cap| cap as usize)
                .unwrap_or(usize::MAX);

            let entry = global_vars
                .entry(array_name.clone())
                .or_insert(Arc::new(Array::new().into()));
            match Arc::make_mut(entry) {
                Value::Array(array) => {
                    // Check here because we don't check on set.
                    if array.len() > cap {
                        error!("[{}] Array {array_name:?} has {} items but is capped at {}. Truncating.", self.name, array.len(), cap);
                        array.truncate(cap);
                    }
                    if array.len() == cap {
                        // It would be more performant to use a rotating index instead,
                        // but I'm not too worried about this micro-optimization (yet).
                        // FIXME
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
            temp_variables
                .entry(name.clone())
                .or_insert_with(|| value.clone());
        }

        Ok(())
    }
}
