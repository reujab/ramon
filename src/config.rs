use anyhow::{anyhow, bail, Error, Result};
use toml::{Table, Value};

pub fn parse(doc: &str) -> Result<Table> {
    let table = doc
        .parse::<Table>()
        .map_err(|err| map_readable_err(doc, err))?;

    // Validate root keys.
    validate_keys(&table, &["function", "monitor", "notify", "var"])?;

    // Validate variables.
    if let Some(vars) = table.get("var").and_then(|vars| vars.as_table()) {
        for (var_name, var) in vars {
            match *var {
                Value::Table(ref var) => validate_keys(var, &["length", "store"])
                    .map_err(|err| anyhow!("Invalid variable `{var_name}`: {err}"))?,
                _ => {}
            }
        }
    }

    // Validate notification config.
    if let Some(_notify_config) = table.get("notify").and_then(|notify| notify.as_table()) {
        // TODO
    }

    // Validate monitors.
    match table.get("monitor") {
        None => {
            bail!("No monitors found!");
        }
        Some(monitors) => {
            let monitors = monitors
                .as_table()
                .ok_or(anyhow!("Key `monitor` must be a table."))?;
            for (name, monitor) in monitors {
                let monitor = monitor
                    .as_table()
                    .ok_or(anyhow!("Monitor `{name}` must be a table."))?;
                validate_keys(monitor, &["log", "match_log", "exec"])
                    .map_err(|err| anyhow!("Monitor `{name}`: {err}"))?;
            }
        }
    }

    Ok(table)
}

/// Turns a `toml::de::Error` into a human-readable error message.
fn map_readable_err(doc: &str, err: toml::de::Error) -> Error {
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
