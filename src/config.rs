use toml::{de::Error, Table, Value};

pub fn parse(doc: &str) -> Result<Table, String> {
    let table = doc
        .parse::<Table>()
        .map_err(|err| map_readable_err(doc, err))?;

    // Validate root keys.
    validate_keys(&table, &vec!["monitor", "notify", "var"])?;

    // Validate variables.
    if let Some(vars) = table.get("var").and_then(|vars| vars.as_table()) {
        for (var_name, var) in vars {
            match *var {
                Value::Table(ref var) => validate_keys(var, &vec!["length", "store"])
                    .map_err(|err| format!("Invalid variable `{var_name}`: {err}"))?,
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
            return Err(format!("No monitors found!"));
        }
        Some(_monitors) => {
            // TODO
        }
    }

    Ok(table)
}

/// Turns a `toml::de::Error` into a human-readable error message.
fn map_readable_err(doc: &str, err: Error) -> String {
    let mut message = err.message().to_owned();
    // Print lines where error occurred.
    if let Some(err_range) = err.span() {
        let mut line_start_byte;
        let mut line_end_byte = 0;
        message += "\n";
        for (i, line) in doc.lines().enumerate() {
            line_start_byte = line_end_byte;
            // Account for new line.
            line_end_byte += line.len() + 1;
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
    message
}

fn validate_keys(table: &Table, valid_keys: &Vec<&'static str>) -> Result<(), String> {
    for key in table.keys() {
        if !valid_keys.contains(&key.as_str()) {
            return Err(format!("Invalid key: {key}"));
        }
    }

    Ok(())
}
