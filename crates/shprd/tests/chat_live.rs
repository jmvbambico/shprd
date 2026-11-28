//! One live chat: a real `pi` in a private `herdr` pane, one turn typed into it,
//! and the same turn read back through `GET /api/chat`.
//!
//! `#[ignore]` because it starts a real `herdr server`, opens a real pane, and
//! spends one model call. Run it by hand with
//!
//! ```text
//! cargo test -p shprd --test chat_live -- --ignored --nocapture
//! ```
//!
//! It is the acceptance test for M10 fix O (gate bug 2): the unit tests prove
//! each program's slug rule and root against fixtures, and this proves the whole
//! wired path — a CLI writes its own session under the real `HOME` it inherited,
//! a service pointed at a different `SHPRD_HOME` finds it, and the chat route
//! answers the person's words and the agent's reply.
//!
//! Nothing real is reachable from here: `HOME`, `HERDR_SOCKET_PATH`, `HC_SESSION`,
//! `HERDR_APP_HOME`, `SHPRD_HOME` and every `XDG_*` variable point into a fresh
//! temporary directory, and the person's own herdr session and Herdr desktop are
//! never addressed. A program that is not installed, or is signed out, skips the
//! test with one line rather than failing it: that is the machine's state, not
//! the fix's.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::Value;

#[path = "../../shprd-mux-herdr/tests/support/live_server.rs"]
mod live_server;

use live_server::LiveServer;
use live_server::herdr_env::isolate;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// The one turn typed into the agent: short, and unmistakable in a transcript.
const TURN: &str = "Reply with the single word BANANA and nothing else. Do not use a tool.";

/// The model the coordinator runs for this test, which is the one this machine's
/// settings name (a provider whose account is signed in), so the skip check asks
/// about the same model the turn spends.
const MODEL: &str = "Command Code/deepseek/deepseek-v4.1-flash";

/// How long the whole exchange gets before the test gives up.
const TIMEOUT: Duration = Duration::from_secs(180);

/// A `shprd serve` child, stopped on drop so a failing test leaves nothing
/// running.
struct Service {
    child: Child,
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs one `git` command in `dir`, failing the test when it does not work.
fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("git must be installed");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// One GET with the bearer token, as `(status, body)`.
fn get(url: &str, token: &str) -> (u16, String) {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build(),
    );
    match agent
        .get(url)
        .header("authorization", &format!("Bearer {token}"))
        .call()
    {
        Ok(mut response) => {
            let status = response.status().as_u16();
            let body = response.body_mut().read_to_string().unwrap_or_default();
            (status, body)
        }
        Err(err) => panic!("the request failed: {err}"),
    }
}

/// The `GET /api/chat` document for `space`, from `since`.
fn chat(base: &str, token: &str, space: &str, since: &str) -> Value {
    let (status, body) = get(
        &format!("{base}/api/chat?space={space}&since={since}"),
        token,
    );
    assert_eq!(status, 200, "GET /api/chat answered {body}");
    serde_json::from_str(&body).expect("the chat is JSON")
}

/// Every message of a page, as `(role, text)`.
fn messages(page: &Value) -> Vec<(String, String)> {
    page["messages"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    (
                        row["role"].as_str().unwrap_or_default().to_string(),
                        row["text"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
#[ignore = "starts a real herdr server and spends one model call; run by hand"]
fn a_live_pi_turn_is_read_back_through_the_chat_route() {
    // A skip is the machine's answer, not a failure: no `herdr`, or a `TMPDIR`
    // too long for a unix socket path. The helper says which, in one line.
    let Some(server) = LiveServer::start() else {
        return;
    };
    // A machine without pi, or with pi signed out, cannot run this test; that is
    // the account's state rather than the thing under test. `pi auth check`
    // needs a provider or a model to ask about, and the model is the one this
    // machine's settings name for a coordinator.
    let signed_in = Command::new("pi")
        .args(["auth", "check", "--model", MODEL, "--no-refresh", "--json"])
        .output();
    match signed_in {
        Ok(output) if output.status.success() => {}
        Ok(_) => {
            eprintln!("skipping the live chat test: pi is not signed in");
            return;
        }
        Err(err) => {
            eprintln!("skipping the live chat test: pi is not installed ({err})");
            return;
        }
    }

    // The home the agent CLI runs with, and the engine's own home, deliberately
    // two different directories: the whole point of the fix is that a transcript
    // is looked for under the first and not the second.
    let agent_home = server.home().join("agent-home");
    let shprd_home = server.home().join("shprd-home");
    for dir in [&agent_home, &shprd_home] {
        std::fs::create_dir_all(dir).unwrap();
    }
    // pi reads its credentials, model catalog and settings from its own agent
    // directory under `HOME`, so a home that is not the person's is a home with
    // no provider: the three files are copied in, and nothing else. Nothing is
    // printed, and the person's own directory is only read.
    seed_pi(&agent_home);

    // A throwaway repository with one commit and an integration branch.
    let repo = server.home().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "live chat\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    git(&repo, &["branch", "integration/m10"]);
    let repo_arg = repo.display().to_string();

    // The isolation rule first: every `HERDR_*`, `HC_*` and `TUIOS_*` name is
    // removed, and the socket is this test's own private server. Every `XDG_*`
    // and `HOME` this test's children see is inside the temp folder.
    let env = |command: &mut Command| {
        isolate(command, &agent_home);
        command
            .env("HOME", &agent_home)
            .env("SHPRD_HOME", &shprd_home)
            .env("XDG_CONFIG_HOME", agent_home.join(".config"))
            .env("XDG_DATA_HOME", agent_home.join(".local").join("share"))
            .env("XDG_STATE_HOME", client_state(&agent_home))
            .env("HERDR_SOCKET_PATH", server.socket())
            .env_remove("SHPRD_DATA_DIR")
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN");
    };

    // The run, and a task added so the repository's space has something to name.
    let shprd = |args: &[&str]| -> String {
        let mut command = Command::new(BIN);
        env(&mut command);
        let output = command
            .args(["--repo", &repo_arg])
            .args(args)
            .output()
            .expect("the shprd binary runs");
        assert!(
            output.status.success(),
            "shprd {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    shprd(&[
        "run",
        "start",
        "--run-id",
        "live",
        "--base",
        "main",
        "--integration",
        "integration/m10",
    ]);
    // The coordinator is pi, chosen by settings and nothing else.
    shprd(&["settings", "set", "roles.coordinator.agent", "pi"]);

    // The repository's own space, which is where the coordinator runs. A person
    // starts the coordinator in that space's first tab, so the test does the
    // same: `shprd serve` records the first tab's root pane as the coordinator's
    // pane, and this is the pane a chat line is typed into.
    let name = format!(
        "shprd \u{b7} {}",
        repo.file_name().unwrap().to_string_lossy()
    );
    server
        .herdr(&[
            "workspace",
            "create",
            "--label",
            &name,
            "--cwd",
            &repo_arg,
            "--env",
            &format!("HOME={}", agent_home.display()),
            "--env",
            &format!(
                "XDG_DATA_HOME={}",
                agent_home.join(".local").join("share").display()
            ),
            "--no-focus",
        ])
        .expect("the test creates its own workspace");
    let pane = first_pane(&server).expect("the space has a pane");

    // `pi` in that pane, waiting for its first turn. `--offline` skips its
    // startup network calls so the test does not depend on them; the model is
    // named outright because a fresh home has no default provider.
    let launch = format!("pi --offline --model {MODEL:?}");
    server
        .herdr(&["pane", "run", &pane, "sh", "-c", &launch])
        .expect("the test starts pi in its own pane");
    // Wait for herdr to *see* the agent before typing: `agent wait` answers
    // "agent_not_found" for a pane it has not detected yet, and a TUI that has
    // not painted would swallow the turn. The poll is on the detection, and the
    // wait is the state.
    let detected = Instant::now() + Duration::from_secs(90);
    loop {
        let agents = server
            .herdr(&["agent", "list"])
            .map(|output| String::from_utf8_lossy(&output.stdout).to_string())
            .unwrap_or_default();
        if agents.contains(&pane) {
            break;
        }
        assert!(
            Instant::now() < detected,
            "herdr never detected pi in {pane}: {agents}\n{}",
            pane_text(&server, &pane)
        );
        sleep(Duration::from_millis(500));
    }
    let ready = server.herdr(&["agent", "wait", &pane, "--timeout", "60000"]);
    assert!(
        ready.is_ok(),
        "pi never reached an agent state: {:?}\n{}",
        ready.err(),
        pane_text(&server, &pane)
    );
    // Type the turn as a person would, then wait for the agent to finish it.
    server
        .herdr(&["pane", "send-text", &pane, TURN])
        .expect("the test types its own turn");
    server
        .herdr(&["pane", "send-keys", &pane, "Enter"])
        .expect("the test submits its own turn");
    let answered = server.herdr(&[
        "agent",
        "wait",
        &pane,
        "--until",
        "idle",
        "--timeout",
        "120000",
    ]);
    let pane_before = pane_text(&server, &pane);
    assert!(
        answered.is_ok() || pane_before.contains("BANANA"),
        "pi never answered: {:?}\n{pane_before}",
        answered.err()
    );

    // The service, pointed at the engine's home (which holds no sessions) and at
    // the private server. Its own transcript roots come from `HOME`, which is
    // the agent home, so it must find the session pi is writing.
    let state = client_state(&agent_home);
    std::fs::create_dir_all(&state).unwrap();
    let mut command = Command::new(BIN);
    env(&mut command);
    let service = Service {
        child: command
            .args([
                "--repo",
                &repo_arg,
                "--home",
                &shprd_home.display().to_string(),
                "serve",
                "--port",
                "0",
                "--json",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the service starts"),
    };
    let record = wait_for_record(&state);
    let port = record["port"].as_u64().expect("a port") as u16;
    let base = format!("http://127.0.0.1:{port}");
    let token = std::fs::read_to_string(service_dir(&state).join("token"))
        .expect("the token is there")
        .trim()
        .to_string();

    // The coordinator's page is read until the whole exchange is in it: the
    // person's turn, and the agent's answer. A program that is starting, or has
    // not flashed its record to disk yet, answers an empty page rather than an
    // error, so the wait is a poll rather than a single read.
    let deadline = Instant::now() + TIMEOUT;
    let mut last: Value = Value::Null;
    let mut cursor = String::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    while Instant::now() < deadline {
        let page = chat(&base, &token, "coordinator", &cursor);
        let more = page["more"].as_bool().unwrap_or(false);
        let gained = messages(&page);
        if !gained.is_empty() {
            cursor = page["cursor"].as_str().unwrap_or_default().to_string();
            seen.extend(gained);
        }
        last = page;
        let replied = seen
            .iter()
            .any(|(role, text)| role == "assistant" && text.contains("BANANA"));
        if replied && !more {
            break;
        }
        sleep(Duration::from_secs(2));
    }

    assert!(
        !seen.is_empty(),
        "the chat never found pi's session under the agent's home {} (last answer: {last}); \
         the engine home {} must not be where it looks. The pane said:\n{}",
        agent_home.display(),
        shprd_home.display(),
        pane_text(&server, &pane)
    );
    assert!(
        seen.iter()
            .any(|(role, text)| role == "user" && text.contains("BANANA")),
        "the person's turn is missing from {seen:?}"
    );
    assert!(
        seen.iter()
            .any(|(role, text)| role == "assistant" && text.contains("BANANA")),
        "pi's reply is missing from {seen:?}"
    );
    assert_eq!(
        last["program"], "pi",
        "the page names the program that wrote it"
    );
    assert!(
        last["session"].as_str().is_some_and(|id| !id.is_empty()),
        "the page names the session it read: {last}"
    );
    // The record really is under the home the CLI ran with, which is what the
    // fix is about; a future regression that looked under `SHPRD_HOME` would
    // fail here even if a stale record made the page look right.
    let sessions = agent_home.join(".pi").join("agent").join("sessions");
    assert!(
        sessions.is_dir(),
        "pi wrote no session directory under {}",
        agent_home.display()
    );
    assert!(
        !shprd_home.join(".pi").exists(),
        "pi wrote under the engine's home, which is not what this test proves"
    );
    drop(service);
}

/// The state directory this test's client verbs use.
fn client_state(agent_home: &Path) -> std::path::PathBuf {
    agent_home.join(".local").join("state")
}

/// The first pane of the space this test created, from `herdr pane list`.
fn first_pane(server: &LiveServer) -> Option<String> {
    let output = server.herdr(&["pane", "list"]).ok()?;
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let value: Value = serde_json::from_str(text.trim()).ok()?;
    let panes = value
        .get("panes")
        .or_else(|| value.get("result").and_then(|result| result.get("panes")))
        .and_then(Value::as_array)?;
    panes.iter().find_map(|pane| {
        pane.get("pane_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
}

/// Waits for the service's discovery record and answers it.
fn wait_for_record(state: &Path) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(service_dir(state).join("service.json"))
            && let Ok(value) = serde_json::from_str(&text)
        {
            return value;
        }
        sleep(Duration::from_millis(50));
    }
    panic!(
        "the service never wrote its record under {}",
        state.display()
    );
}

/// Copies the person's pi credentials, model catalog and settings into
/// `home/.pi/agent`, so a private `HOME` can run the same model.
///
/// Only those three files are read, and only to be copied: the person's own
/// agent directory is never written to, and an installation without them skips
/// the test (the caller already checked `pi auth check`). Nothing here prints a
/// key.
fn seed_pi(home: &Path) {
    let Some(source) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return;
    };
    let from = source.join(".pi").join("agent");
    let to = home.join(".pi").join("agent");
    if std::fs::create_dir_all(&to).is_err() {
        return;
    }
    for name in ["auth.json", "models.json", "settings.json"] {
        let file = from.join(name);
        if file.is_file() {
            let _ = std::fs::copy(&file, to.join(name));
        }
    }
}

/// What a pane's screen says, for a failure message.
fn pane_text(server: &LiveServer, pane: &str) -> String {
    server
        .herdr(&["pane", "read", pane, "--lines", "40", "--format", "text"])
        .map(|output| String::from_utf8_lossy(&output.stdout).to_string())
        .unwrap_or_else(|err| format!("(the pane could not be read: {err})"))
}

/// The one service's directory under this state home.
fn service_dir(state: &Path) -> PathBuf {
    state.join("shprd")
}
