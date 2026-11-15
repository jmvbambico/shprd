//! The graph's terminal: the one the Herdr window's graph block shows.
//!
//! `shprd serve` starts it, before a window can load the layout that refers to
//! it, and the block in the window is `{ "terminal": "shprd-graph" }`: a terminal
//! running `dagr view <repo>/.dagr/run.json` in a shell loop, so a dagr that
//! exits is restarted in place and the terminal's address never changes.
//!
//! Why the service and not the mod in the window: Herdr resolves a layout
//! terminal block by name and **prunes the block from the layout file** when no
//! such terminal exists when the layout loads (measured), writing the shortened
//! file back. A block that is supposed to start the terminal itself would
//! therefore be gone before it could — the terminal has to exist first, and the
//! service is the only part of shprd that is up before the window is.
//!
//! It is started only when the host probe found a Herdr desktop — a window that
//! answered and a core to run terminals in. A machine that drives the herdr TUI
//! has no window to draw a block in, and asking a core anything starts one
//! (M7ef §2.3), so nothing is started there. That is also what keeps every test
//! quiet: no window answers in a test, so the probe never says desktop.
//!
//! The mod keeps its own start as a fallback (a service started before the
//! window's core, a terminal someone ended), and the note it draws is what a
//! person sees when there is no dagr at all.

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

use shprd_mux_hc::Endpoint;
use shprd_server::graph::{Dagr, run_json};

use crate::host::Resolution;

/// The name the layout's block resolves, and the terminal this starts.
pub const TERMINAL: &str = "shprd-graph";

/// What to do about the block's terminal, from the core's own list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Start a terminal, forgetting these finished ones first: Herdr resolves a
    /// block's name to the *first* match, and a finished terminal keeps its name.
    Start {
        /// Addresses to forget with `hc rm`, in list order.
        forget: Vec<String>,
    },
    /// A live terminal already draws this run.
    Reuse {
        /// Its address, for the log line.
        address: String,
    },
    /// A live terminal with the block's name draws something else: leave it, and
    /// say so rather than starting a second one that could not take the name.
    Leave {
        /// Why, in words a person can read.
        reason: String,
    },
    /// Nothing to start.
    Skip {
        /// Why, in words a person can read.
        reason: String,
    },
}

/// Decides from `hc ls --all --json`'s terminals whether the block's terminal has
/// to be started.
///
/// `run_json` is the document the block should be drawing, as a path string: a
/// terminal is running this run when its argv names it, which is the same rule
/// the window's mod applies.
pub fn plan(terminals: &[Value], run_json: &str) -> Plan {
    let named: Vec<&Value> = terminals
        .iter()
        .filter(|terminal| terminal["name"].as_str() == Some(TERMINAL))
        .collect();
    let live: Vec<&&Value> = named
        .iter()
        .filter(|terminal| terminal["state"].as_str() != Some("finished"))
        .collect();
    if let Some(terminal) = live.iter().find(|terminal| names(terminal, run_json)) {
        return Plan::Reuse {
            address: terminal["address"].as_str().unwrap_or_default().to_string(),
        };
    }
    if let Some(other) = live.first() {
        let argv = other["argv"].to_string();
        return Plan::Leave {
            reason: format!("`{TERMINAL}` is already running {argv}"),
        };
    }
    if run_json.trim().is_empty() {
        return Plan::Skip {
            reason: "there is no document to draw".to_string(),
        };
    }
    Plan::Start {
        forget: named
            .iter()
            .filter(|terminal| terminal["state"].as_str() == Some("finished"))
            .filter_map(|terminal| terminal["address"].as_str().map(str::to_string))
            .collect(),
    }
}

/// Whether a terminal's argv names `run_json`.
fn names(terminal: &Value, run_json: &str) -> bool {
    match terminal["argv"].as_array() {
        Some(argv) => argv
            .iter()
            .filter_map(Value::as_str)
            .any(|part| part.contains(run_json)),
        None => false,
    }
}

/// The command that keeps the document on screen.
///
/// It is `dagr view <run.json>`, dagr's own follow mode — it watches the file and
/// redraws when the service rewrites it — under a shell that restarts it when it
/// exits, and waits for the document when the run has not been projected yet. The
/// loop is what keeps the terminal (and so the block's address) alive.
pub fn argv(dagr: &Path, run_json: &Path) -> Vec<String> {
    vec![
        "sh".to_string(),
        "-c".to_string(),
        "while [ ! -f \"$1\" ]; do sleep 1; done; \
         while :; do \"$0\" view \"$1\"; sleep 1; done"
            .to_string(),
        dagr.display().to_string(),
        run_json.display().to_string(),
    ]
}

/// Makes sure the window has the graph's terminal, starting it when the desktop
/// is there and nothing is drawing this run.
///
/// Every failure is a warning and never fatal: a run whose graph cannot be drawn
/// is still a run, and a service that refused to start over a missing window
/// would be worse than a block with nothing in it.
pub fn ensure(resolution: &Resolution, dagr: &Dagr, repo: &Path) {
    if !resolution.probe.desktop() {
        tracing::debug!(
            why = %resolution.why,
            "not starting the graph's terminal: no Herdr desktop to draw it in"
        );
        return;
    }
    let Some(located) = dagr.located() else {
        // No dagr: the window's note is what says so, with the setting to fix.
        return;
    };
    let document = run_json(repo);
    let endpoint = crate::host::endpoint();
    let listing = match hc(&endpoint, &["ls", "--all", "--json"]) {
        Ok(listing) => listing,
        Err(err) => {
            tracing::warn!(%err, "could not list herdr-core's terminals for the graph block");
            return;
        }
    };
    let terminals: Vec<Value> = listing["terminals"].as_array().cloned().unwrap_or_default();
    match plan(&terminals, &document.display().to_string()) {
        Plan::Start { forget } => {
            for address in forget {
                if let Err(err) = hc(&endpoint, &["rm", &address]) {
                    tracing::warn!(%address, %err, "could not forget a finished graph terminal");
                }
            }
            let mut args: Vec<String> = vec![
                "run".to_string(),
                "--bg".to_string(),
                "--no-parent".to_string(),
                "--name".to_string(),
                TERMINAL.to_string(),
                "--".to_string(),
            ];
            args.extend(argv(&located.path, &document));
            match hc(&endpoint, &args) {
                Ok(_) => tracing::info!(
                    document = %document.display(),
                    "started the graph's terminal for the window"
                ),
                Err(err) => {
                    tracing::warn!(%err, "could not start the graph's terminal");
                }
            }
        }
        Plan::Reuse { address } => tracing::debug!(%address, "the graph's terminal is already up"),
        Plan::Leave { reason } => tracing::warn!(%reason, "the graph block shows another run"),
        Plan::Skip { reason } => tracing::debug!(%reason, "nothing to draw yet"),
    }
}

/// One `hc` call with this session's core, its stdout parsed as JSON.
///
/// The environment is the endpoint's own — `$SHPRD_HC` picks the binary,
/// `HC_SESSION`/`HC_SOCKET` the core and the five XDG directories where it keeps
/// its socket — and every `HC_*` variable of this process is dropped first, so a
/// service started in one session never drives another's core.
fn hc(endpoint: &Endpoint, args: &[impl AsRef<std::ffi::OsStr>]) -> Result<Value, String> {
    let mut command = Command::new(&endpoint.binary);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for variable in Endpoint::DROPPED {
        command.env_remove(variable);
    }
    for (name, value) in endpoint.environment() {
        command.env(name, value);
    }
    let output = command
        .output()
        .map_err(|err| format!("running {}: {err}", endpoint.binary))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("{} exited {}", endpoint.binary, output.status)
        } else {
            stderr
        });
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text)
        .map_err(|err| format!("{} answered no JSON: {err}", endpoint.binary))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;

    /// The document a run projects to, as the block names it.
    const DOC: &str = "/repo/.dagr/run.json";

    /// One `hc ls` terminal.
    fn terminal(address: &str, state: &str, argv: &[&str]) -> Value {
        json!({
            "address": address,
            "name": TERMINAL,
            "state": state,
            "argv": argv,
        })
    }

    /// The argv of a running graph terminal, for `document`.
    fn drawing(document: &str) -> Vec<String> {
        argv(Path::new("/opt/dagr"), Path::new(document))
    }

    #[test]
    fn nothing_running_starts_it() {
        let terminals = vec![terminal(
            "t3",
            "finished",
            &drawing(DOC).iter().map(String::as_str).collect::<Vec<_>>(),
        )];
        assert_eq!(
            plan(&terminals, DOC),
            Plan::Start {
                forget: vec!["t3".to_string()]
            },
            "a finished terminal keeps the name, so it is forgotten first"
        );
        assert_eq!(
            plan(&[], DOC),
            Plan::Start { forget: Vec::new() },
            "nothing there at all is a plain start"
        );
    }

    #[test]
    fn a_live_terminal_drawing_this_run_is_reused() {
        let terminals = vec![terminal(
            "t9",
            "live",
            &drawing(DOC).iter().map(String::as_str).collect::<Vec<_>>(),
        )];
        assert_eq!(
            plan(&terminals, DOC),
            Plan::Reuse {
                address: "t9".to_string()
            },
            "a service restarted beside its own terminal must not start a second"
        );
    }

    #[test]
    fn a_live_terminal_drawing_another_run_is_left_alone() {
        let terminals = vec![terminal(
            "t9",
            "live",
            &drawing("/elsewhere/.dagr/run.json")
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        )];
        match plan(&terminals, DOC) {
            Plan::Leave { reason } => assert!(reason.contains("elsewhere"), "{reason}"),
            other => panic!("expected Leave, got {other:?}"),
        }
    }

    #[test]
    fn terminals_of_other_names_are_not_the_block() {
        let terminals = vec![json!({
            "address": "t1",
            "name": "shprd-serve",
            "state": "live",
            "argv": ["shprd", "serve"],
        })];
        assert_eq!(plan(&terminals, DOC), Plan::Start { forget: Vec::new() });
    }

    #[test]
    fn the_terminal_runs_dagrs_follow_mode_under_a_shell() {
        let command = argv(Path::new("/opt/dagr"), Path::new(DOC));
        assert_eq!(command[0], "sh");
        assert_eq!(command[1], "-c");
        assert!(
            command[2].contains("view"),
            "dagr's own view: {}",
            command[2]
        );
        assert!(
            command[2].contains("[ ! -f"),
            "it waits for the document a run has not projected yet"
        );
        assert_eq!(command[3], "/opt/dagr");
        assert_eq!(command[4], DOC);
    }
}
