use agentty_core::core::config;
use agentty_core::daemon::{pidfile, spawn};
use anyhow::{Result, bail};
use serde_json::json;

use crate::commands::{Outcome, Report};

const LOG_TAIL_LINES: usize = 40;

fn report(human: impl Into<String>, json: serde_json::Value) -> Result<Outcome> {
    Ok(Outcome::Report(Report {
        human: human.into(),
        json,
    }))
}

pub fn start() -> Result<Outcome> {
    if spawn::is_ready() {
        return report(
            "the server is already running",
            json!({ "started": false, "running": true, "pid": pidfile::read() }),
        );
    }

    let executable = spawn::daemon_executable()?;
    spawn::ensure_running()?;
    if !spawn::is_ready() {
        bail!(
            "the local runtime is reachable but not ready after start; inspect agentty server logs"
        );
    }
    let pid = pidfile::read();
    report(
        match pid {
            Some(pid) => format!("started {} (pid {pid})", executable.display()),
            None => format!("started {}", executable.display()),
        },
        json!({
            "started": true,
            "pid": pid,
            "exe": executable.display().to_string(),
        }),
    )
}

pub fn stop() -> Result<Outcome> {
    let was_reachable = spawn::is_reachable();
    spawn::stop();
    if spawn::is_reachable() {
        bail!("the server did not shut down on request");
    }
    if !was_reachable {
        return report(
            "the server is not running",
            json!({ "stopped": false, "running": false }),
        );
    }
    report("stopped", json!({ "stopped": true }))
}

pub fn restart() -> Result<Outcome> {
    let executable = spawn::daemon_executable()?;
    spawn::restart()?;
    if !spawn::is_ready() {
        bail!(
            "the local runtime is reachable but not ready after restart; inspect agentty server logs"
        );
    }
    let pid = pidfile::read();
    report(
        match pid {
            Some(pid) => format!("restarted {} (pid {pid})", executable.display()),
            None => format!("restarted {}", executable.display()),
        },
        json!({
            "restarted": true,
            "pid": pid,
            "exe": executable.display().to_string(),
        }),
    )
}

pub fn logs() -> Result<Outcome> {
    let Some(path) = config::config_path("agentty.log") else {
        bail!("no config directory, so no log file location");
    };
    let mut human = format!("{}\n", path.display());
    let mut lines: Vec<String> = Vec::new();
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            lines = contents
                .lines()
                .rev()
                .take(LOG_TAIL_LINES)
                .map(str::to_string)
                .collect();
            lines.reverse();
            for line in &lines {
                human.push_str(line);
                human.push('\n');
            }
        }
        Err(_) => {
            human.push_str(
                "no warning or error has been logged yet — set AGENTTY_LOG=info for verbose server diagnostics\n",
            );
        }
    }
    report(
        human,
        json!({ "path": path.display().to_string(), "lines": lines }),
    )
}
