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
    /// A live terminal carries the block's name, and what it runs has to be read
    /// before it can be said whether that is this run: `hc ls` does not list a
    /// terminal's argv on hc 0.1.21 (measured — `hc get` does).
    Read {
        /// Its address, to ask about.
        address: String,
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
/// The list is the cheap question — name and state, which every `hc` answers — and
/// that is what most of this needs: a finished terminal to forget, no terminal at
/// all, or nothing to draw. What a live one is *running* is the expensive
/// question, and on hc 0.1.21 the list does not carry it (measured: a live
/// terminal's record there is address, agent, cwd, name, state and turn, with no
/// argv at all), so a live terminal with the block's name comes back as
/// [`Plan::Read`] and [`decide`] settles it from `hc get`'s record.
pub fn plan(terminals: &[Value], run_json: &str) -> Plan {
    let named: Vec<&Value> = terminals
        .iter()
        .filter(|terminal| terminal["name"].as_str() == Some(TERMINAL))
        .collect();
    if let Some(terminal) = named
        .iter()
        .find(|terminal| terminal["state"].as_str() != Some("finished"))
    {
        let address = terminal["address"].as_str().unwrap_or_default().to_string();
        return match names(terminal, run_json) {
            Some(true) => Plan::Reuse { address },
            Some(false) => Plan::Leave {
                reason: format!("`{TERMINAL}` is already running {}", terminal["argv"]),
            },
            // The list did not say, which is what hc 0.1.21's answer looks like.
            None => Plan::Read { address },
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

/// What one live terminal's own record (`hc get <address> --json`) says about it.
///
/// `Reuse` when its argv names this run's document, `Leave` when it draws another,
/// and `Reuse` again when the record still carries no argv: the name is taken
/// either way, so a second terminal could not have it, and the window's note —
/// which reads the *app's* terminal list, argv included — is what tells a person
/// when a graph is showing another repository.
pub fn decide(address: &str, record: &Value, run_json: &str) -> Plan {
    match names(record, run_json) {
        Some(true) => Plan::Reuse {
            address: address.to_string(),
        },
        Some(false) => Plan::Leave {
            reason: format!("`{TERMINAL}` is already running {}", record["argv"]),
        },
        None => Plan::Reuse {
            address: address.to_string(),
        },
    }
}

/// Whether a terminal's argv names `run_json`, or `None` when the record carries
/// no argv to read.
///
/// The third answer is what hc 0.1.21's `ls` gives for every terminal, so it is a
/// fact about the tool and not an error: a caller that needs the answer asks
/// `hc get`, which does carry it.
fn names(record: &Value, run_json: &str) -> Option<bool> {
    let argv = record.get("argv")?.as_array()?;
    let parts: Vec<&str> = argv.iter().filter_map(Value::as_str).collect();
    if parts.is_empty() {
        return None;
    }
    Some(parts.iter().any(|part| part.contains(run_json)))
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
    let decided = match plan(&terminals, &document.display().to_string()) {
        // A live terminal has the name; `hc get` is the verb that says what it is
        // running. Without it the service cannot tell its own graph from another
        // run's, and starting a second terminal is not an option (the name is
        // taken), so a read that fails leaves the terminal as it is.
        Plan::Read { address } => match hc(&endpoint, &["get", &address, "--json"]) {
            Ok(record) => decide(&address, &record, &document.display().to_string()),
            Err(err) => {
                tracing::warn!(
                    %address,
                    %err,
                    "could not read the graph terminal's argv; leaving it as it is"
                );
                return;
            }
        },
        decided => decided,
    };
    match decided {
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
        Plan::Reuse { address } => {
            tracing::debug!(%address, "the graph's terminal is already up");
        }
        Plan::Leave { reason } => tracing::info!(%reason, "the graph block shows another run"),
        Plan::Skip { reason } => tracing::debug!(%reason, "nothing to draw yet"),
        // Settled above: a live terminal always comes back as Reuse or Leave.
        Plan::Read { address } => {
            tracing::debug!(%address, "the graph's terminal was not read");
        }
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

    /// One `hc ls --all --json` terminal, exactly as hc 0.1.21 answers: address,
    /// agent, cwd, name, state and turn — **no argv**, live or finished. A test
    /// that invented one here would hide the reason [`Plan::Read`] exists.
    fn listed(address: &str, state: &str) -> Value {
        json!({
            "address": address,
            "agent": "sh",
            "cwd": "/repo",
            "name": TERMINAL,
            "state": state,
            "turn": 0,
        })
    }

    /// The `hc get <address> --json` record of the block's terminal, running
    /// `document`.
    fn got(address: &str, document: &str) -> Value {
        let command = argv(Path::new("/opt/dagr"), Path::new(document));
        json!({
            "address": address,
            "name": TERMINAL,
            "state": "live",
            "argv": command,
        })
    }

    #[test]
    fn nothing_running_starts_it() {
        let terminals = vec![listed("t3", "finished")];
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
        assert_eq!(
            plan(&terminals, "  "),
            Plan::Skip {
                reason: "there is no document to draw".to_string()
            },
            "a finished one and nothing to draw is no start"
        );
    }

    #[test]
    fn a_live_terminal_the_list_does_not_describe_is_read() {
        // The answer hc 0.1.21 gives: a live terminal with no argv in the list,
        // so whether it is this run's is a second question.
        let terminals = vec![listed("t9", "live")];
        assert_eq!(
            plan(&terminals, DOC),
            Plan::Read {
                address: "t9".to_string()
            },
            "hc ls carries no argv on this hc, so it cannot be decided from the list"
        );
    }

    #[test]
    fn a_list_that_carries_argv_settles_it_without_a_second_call() {
        let ours = got("t9", DOC);
        assert_eq!(
            plan(&[ours], DOC),
            Plan::Reuse {
                address: "t9".to_string()
            },
            "a core that does list argv is answered from the list"
        );
        let theirs = got("t9", "/elsewhere/.dagr/run.json");
        match plan(&[theirs], DOC) {
            Plan::Leave { reason } => assert!(reason.contains("elsewhere"), "{reason}"),
            other => panic!("expected Leave, got {other:?}"),
        }
    }

    #[test]
    fn the_get_record_says_whether_it_draws_this_run() {
        assert_eq!(
            decide("t9", &got("t9", DOC), DOC),
            Plan::Reuse {
                address: "t9".to_string()
            },
            "a restarted service beside its own terminal reuses it"
        );
        match decide("t9", &got("t9", "/elsewhere/.dagr/run.json"), DOC) {
            Plan::Leave { reason } => assert!(reason.contains("elsewhere"), "{reason}"),
            other => panic!("expected Leave, got {other:?}"),
        }
        // A record that still will not say: the name is taken either way, so a
        // second terminal cannot start, and the window's note is what reports a
        // graph showing another repository.
        assert_eq!(
            decide("t9", &listed("t9", "live"), DOC),
            Plan::Reuse {
                address: "t9".to_string()
            },
            "an unreadable argv is not a reason to call the block wrong"
        );
    }

    #[test]
    fn terminals_of_other_names_are_not_the_block() {
        let terminals = vec![json!({
            "address": "t1",
            "name": "shprd-serve",
            "state": "live",
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
