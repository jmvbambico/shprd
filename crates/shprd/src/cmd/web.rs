//! `shprd web`: the login password and the sessions it starts.
//!
//! This is the only way the password is ever set. The web app is a shell behind
//! the password, so a web page — or anything a tunnel can reach — must never be
//! able to set or reset it (M7d §4.4: "recovery is the local CLI/token"). The
//! commands here read and write only the service's state directory:
//! `<state>/shprd/web-auth.json` and `<state>/shprd/sessions.json`.
//!
//! **The password is typed, not printed, and never echoed**: a terminal gets
//! `stty -echo` for the two prompts, and a script may pipe the two lines instead
//! (that is how the tests drive it). Every way in refuses rather than guesses: a
//! terminal whose echo cannot be turned off is a refusal, and so is a password
//! that is too short, too common or this machine's own name.
//!
//! **A session list never shows a session id.** What `sessions` prints is the
//! first twelve characters of the *hash* the service stores, which is what
//! `--revoke` takes; the id itself lives only in the browser's cookie, so a list
//! that leaked cannot be used to sign in.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::anyhow;
use serde_json::{Value, json};
use shprd_core::log::SystemClock;
use shprd_remote::{PasswordStore, RemoteError, Revoked, SessionRecord, Sessions};

use crate::cli::{PasswordCommand, SessionsArgs, WebCommand};
use crate::ctx::{CliError, resolve_home, resolve_state_home};

/// Runs one `shprd web` verb.
pub fn run(cmd: &WebCommand, home: Option<&Path>, json: bool) -> Result<(), CliError> {
    let dir = state_dir(home)?;
    match cmd {
        WebCommand::Password { cmd } => password(cmd, &dir, json),
        WebCommand::Sessions(args) => sessions(args, &dir, json),
        WebCommand::Up(args) => super::web_tunnel::up(args, &dir, json),
        WebCommand::Down => super::web_tunnel::down(&dir, json),
        WebCommand::Status(args) => super::web_tunnel::status(args, &dir, json),
    }
}

/// The service's own state directory, resolved once at this edge.
fn state_dir(home: Option<&Path>) -> Result<PathBuf, CliError> {
    let home = resolve_home(home)?;
    Ok(shprd_server::state_dir(&resolve_state_home(&home)))
}

/// `shprd web password set|clear`.
fn password(cmd: &PasswordCommand, dir: &Path, json: bool) -> Result<(), CliError> {
    let store = PasswordStore::new(dir);
    match cmd {
        PasswordCommand::Set => {
            let first = ask_hidden("New password: ")?;
            let again = ask_hidden("Again: ")?;
            if first != again {
                return Err(CliError::Invalid(anyhow!(
                    "the two passwords differ; nothing was changed"
                )));
            }
            PasswordStore::check_rules(&first, shprd_remote::password::hostname().as_deref())
                .map_err(|refusal| CliError::Invalid(anyhow!(refusal)))?;
            store.set(&first).map_err(failed)?;
            // Every session trusted the old password. A change that left them
            // alive would keep a stolen cookie working, which is the one thing a
            // password change is for.
            let revoked = sessions_store(dir).revoke_all().map_err(failed)?;
            if json {
                print_json(json!({"ok": true, "password": "set", "revoked": revoked}))?;
            } else {
                println!(
                    "the login password is set; {revoked} session{} ended",
                    plural(revoked)
                );
            }
            Ok(())
        }
        PasswordCommand::Clear => {
            store.clear().map_err(failed)?;
            let revoked = sessions_store(dir).revoke_all().map_err(failed)?;
            if json {
                print_json(json!({"ok": true, "password": "cleared", "revoked": revoked}))?;
            } else {
                println!(
                    "the login password is cleared: the web can no longer sign in; \
                     the loopback token still works. {revoked} session{} ended",
                    plural(revoked)
                );
            }
            Ok(())
        }
    }
}

/// `shprd web sessions [--revoke <ID> | --revoke-all]`.
fn sessions(args: &SessionsArgs, dir: &Path, json: bool) -> Result<(), CliError> {
    let store = sessions_store(dir);
    if args.revoke_all {
        let revoked = store.revoke_all().map_err(failed)?;
        return if json {
            print_json(json!({"ok": true, "revoked": revoked}))
        } else {
            println!("{revoked} session{} ended", plural(revoked));
            Ok(())
        };
    }
    if let Some(id) = &args.revoke {
        return match store.revoke_by_id(id).map_err(failed)? {
            Revoked::Removed(count) => {
                if json {
                    print_json(json!({"ok": true, "revoked": count}))
                } else {
                    println!("{count} session ended");
                    Ok(())
                }
            }
            Revoked::Nothing => Err(CliError::Invalid(anyhow!(
                "no session starts with {id:?}: `shprd web sessions` lists them"
            ))),
            Revoked::Ambiguous(count) => Err(CliError::Invalid(anyhow!(
                "{id:?} names {count} sessions; give more of the id, or use --revoke-all"
            ))),
        };
    }

    let list = store.list().map_err(failed)?;
    if json {
        let rows: Vec<Value> = list.iter().map(row_json).collect();
        return print_json(json!({ "ok": true, "sessions": rows }));
    }
    if list.is_empty() {
        println!("no sessions");
        return Ok(());
    }
    let now = store.now();
    for session in &list {
        println!("{}", row_line(session, now));
    }
    println!(
        "\n{} session{}; `--revoke <id>` ends one, `--revoke-all` ends them all",
        list.len(),
        plural(list.len())
    );
    Ok(())
}

/// The session store over `dir`, on the real clock.
fn sessions_store(dir: &Path) -> Sessions {
    Sessions::new(dir, Arc::new(SystemClock))
}

/// One session as a line a person reads.
///
/// Times are epoch seconds, printed as seconds: the CLI has no date formatting of
/// its own, and a machine-readable answer is what `--json` is for.
fn row_line(session: &SessionRecord, now: u64) -> String {
    format!(
        "{}  {:<24} {}  last seen {}{}{}  ends {}",
        session.short_id(),
        session.label,
        if session.remember {
            "remembered"
        } else {
            "ordinary "
        },
        session.last_seen,
        if session.fresh(now) { " (fresh)" } else { "" },
        if session.expired(now) { " EXPIRED" } else { "" },
        session.expires_at(),
    )
}

/// One session as JSON, for `--json`.
fn row_json(session: &SessionRecord) -> Value {
    json!({
        "id": session.short_id(),
        "label": session.label,
        "ua_class": session.ua_class,
        "remember": session.remember,
        "created": session.created,
        "last_seen": session.last_seen,
        "fresh_until": session.fresh_until,
        "expires_at": session.expires_at(),
    })
}

/// Prints one JSON value on stdout.
fn print_json(value: Value) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(&value)
        .map_err(|err| CliError::other(anyhow!(err).context("serializing the answer")))?;
    println!("{text}");
    Ok(())
}

/// Wraps a [`RemoteError`] as a verb that did not happen.
fn failed(err: RemoteError) -> CliError {
    CliError::other(anyhow!(err))
}

/// `n`'s plural `s`, so a line reads "1 session" or "2 sessions".
fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Reads a password without echoing it.
///
/// A terminal gets a prompt and `stty -echo` for the duration; a pipe reads one
/// line and is how a script or a test answers. A terminal whose echo cannot be
/// turned off is a refusal: a password on the screen is a password in a
/// screenshot, a screen share and a scrollback.
fn ask_hidden(prompt: &str) -> Result<String, CliError> {
    let interactive = std::io::stdin().is_terminal();
    // A pipe gets no prompt: the two lines are the whole conversation, and a
    // script that reads stdout should not have to strip one.
    if interactive {
        print!("{prompt}");
        std::io::stdout()
            .flush()
            .map_err(|err| CliError::other(anyhow!(err).context("writing the prompt")))?;
    }

    let mut line = String::new();
    if interactive {
        let echo = EchoOff::new()?;
        let read = std::io::stdin().lock().read_line(&mut line);
        drop(echo);
        println!();
        read.map_err(|err| CliError::other(anyhow!(err).context("reading the password")))?;
    } else {
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|err| CliError::other(anyhow!(err).context("reading the password")))?;
    }
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

/// Terminal echo, off until this is dropped.
struct EchoOff {
    program: PathBuf,
}

impl EchoOff {
    /// Turns echo off, or refuses if this terminal cannot.
    fn new() -> Result<Self, CliError> {
        let program = ["/bin/stty", "/usr/bin/stty"]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
            .ok_or_else(|| {
                CliError::other(anyhow!(
                    "no `stty` here, so this terminal cannot hide what you type: \
                     pipe the password in instead, or use one that can"
                ))
            })?;
        if !stty(&program, "-echo") {
            return Err(CliError::other(anyhow!(
                "this terminal will not turn off echo, and a password on the screen is \
                 a password in a screenshot: run this in a terminal that can, or pipe \
                 the password in instead"
            )));
        }
        Ok(Self { program })
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        // The password is already in hand; failing to restore echo is worth a
        // line, not a panic, and the terminal is the person's to fix.
        if !stty(&self.program, "echo") {
            eprintln!("could not turn terminal echo back on; run `stty echo`");
        }
    }
}

/// Runs `stty <flag>` on this process's terminal, answering whether it worked.
fn stty(program: &Path, flag: &str) -> bool {
    std::process::Command::new(program)
        .arg(flag)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
