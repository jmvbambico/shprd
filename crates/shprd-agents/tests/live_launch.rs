//! Real launches, one per program, run by hand at a milestone gate.
//!
//! Run them with:
//!
//! ```text
//! cargo test -p shprd-agents --test live_launch -- --ignored --test-threads=1
//! ```
//!
//! Each test builds a launch with [`shprd_agents::adapter`] from the shipped
//! harness, points it at a temp directory, and hands it a brief that asks for one
//! word back. The reply is the only thing that costs tokens, and it is the point:
//! the word comes back only when the brief actually reached the program, so a
//! broken brief flag, model flag or effort flag fails the test instead of looking
//! fine in a dry run.
//!
//! The launch is the program's non-interactive one — `claude -p`,
//! `codex exec --skip-git-repo-check`, `pi -p`, `opencode run`, `kilo run` —
//! because a terminal UI cannot be driven from a test and a brand-new directory
//! shows Claude Code's folder-trust dialog before any turn. `codex exec` also
//! refuses a directory that is not a git repository unless it is told to skip
//! that check, which is how the program's own one-shot form is started.
//! Everything else is what [`shprd_agents::adapter::launch`] builds: the
//! harness's model, effort and brief arguments. The access mode's arguments are
//! left out: a one-shot prompt approves nothing, and `codex exec` does not take
//! `-a` at all.
//!
//! An account that is out of usage skips its test: the program's own error is
//! matched against the dry and rate-limit phrases [`shprd_agents::exhausted`]
//! knows, and a match prints `skipped: <program> out of quota` and returns. That
//! is the account's state, not the harness's, and a milestone gate must not read
//! as a harness failure on a machine whose accounts are not all paid up.
//!
//! Each CLI must be installed and signed in; a program that is not installed
//! fails its test with that reason. Nothing is written outside the temp
//! directory except the CLI's own session record under the person's home.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use shprd_agents::adapter::{Brief, BriefRoute, Host, Pick, launch};
use shprd_agents::env::{EnvContext, Role};
use shprd_agents::exhausted::exhausted;
use shprd_agents::harness::Harness;

/// How long one program gets to answer before it is stopped.
const TIMEOUT: Duration = Duration::from_secs(180);

/// The words a role hands the program; the reply proves they arrived.
const BRIEF: &str = "When this is read, reply with the single word BANANA and \
                     nothing else. Do not use a tool.";

/// The first turn a program gets when the brief went in as its system prompt.
const KICKOFF: &str = "Start now: follow the brief in your system prompt.";

/// The harness `id` from `data/harness/`.
fn harness(id: &str) -> Harness {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/harness")
        .join(format!("{id}.toml"));
    match Harness::load(&path) {
        Ok(harness) => harness,
        Err(error) => panic!("{id} should load: {error}"),
    }
}

/// The non-interactive base command for each program, and whether it takes the
/// first turn as its last argument (all of these do).
fn one_shot_base(id: &str) -> Vec<String> {
    let words: &[&str] = match id {
        "claude" => &["claude", "-p"],
        // The temp dir is not a git repository, and codex exec refuses to run in
        // one unless it is told to skip that check.
        "codex" => &["codex", "exec", "--skip-git-repo-check"],
        "pi" => &["pi", "-p"],
        "opencode" => &["opencode", "run"],
        "kilo" => &["kilo", "run"],
        other => panic!("no one-shot command for {other}"),
    };
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// The one-shot argv: the adapter's launch, with the program's non-interactive
/// base and without the access mode's arguments.
fn one_shot(harness: &Harness, argv: &[String], prompt: &str) -> Vec<String> {
    let base = harness.spec().launch.interactive.len();
    let mut out = one_shot_base(harness.id());
    out.extend(argv[base.min(argv.len())..].iter().cloned());
    if let Some(mode) = harness.default_permission()
        && !mode.args.is_empty()
        && let Some(start) = out
            .windows(mode.args.len())
            .position(|window| window == mode.args.as_slice())
    {
        out.drain(start..start + mode.args.len());
    }
    out.push(prompt.to_owned());
    out
}

/// Whether the binary at the front of `argv` is on the path.
fn installed(program: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v '{program}'")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Start `argv` in `dir` and wait up to [`TIMEOUT`] for it to end.
///
/// Returns its stdout and stderr, so a failing reply can be read in the panic
/// message.
fn run(argv: &[String], dir: &Path) -> (std::process::ExitStatus, String, String) {
    let mut parts = argv.iter();
    let program = match parts.next() {
        Some(program) => program,
        None => panic!("the argv is empty"),
    };
    let child = Command::new(program)
        .args(parts)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let child = match child {
        Ok(child) => child,
        Err(error) => panic!("{program} should start: {error}"),
    };
    let output = wait_with_timeout(child, argv);
    let stdout = String::from_utf8_lossy(&output.0).into_owned();
    let stderr = String::from_utf8_lossy(&output.1).into_owned();
    (output.2, stdout, stderr)
}

/// Wait for `child`, killing it once [`TIMEOUT`] passes.
fn wait_with_timeout(
    mut child: Child,
    argv: &[String],
) -> (Vec<u8>, Vec<u8>, std::process::ExitStatus) {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > TIMEOUT => {
                let _ = child.kill();
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(error) => panic!("waiting for {argv:?} failed: {error}"),
        }
    }
    match child.wait_with_output() {
        Ok(output) => (output.stdout, output.stderr, output.status),
        Err(error) => panic!("reading the output of {argv:?} failed: {error}"),
    }
}

/// Launch `id` once with the shipped harness and check the brief came back.
fn live(id: &str) {
    assert!(
        installed(id),
        "{id} is not on the PATH, so there is nothing to launch"
    );
    let harness = harness(id);
    let dir = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => panic!("a temp dir should be created: {error}"),
    };
    let brief_path = dir.path().join("brief.md");
    if let Err(error) = std::fs::write(&brief_path, BRIEF) {
        panic!("the brief should be written: {error}");
    }
    let brief = Brief {
        text: BRIEF.to_owned(),
        file: brief_path,
    };
    // No model, effort or fast: the program's own defaults, so the test proves
    // the launch at all before proving a particular catalogue row.
    let pick = Pick {
        program: id.to_owned(),
        ..Pick::default()
    };
    let base_env: BTreeMap<String, String> = std::env::vars().collect();
    let ctx = EnvContext {
        actor: format!("live-test:{id}"),
        task: None,
        run: None,
        repo: dir.path().to_owned(),
        home: None,
        data: None,
    };
    let launch = match launch(
        &harness,
        &pick,
        &brief,
        &Host {
            base_env: &base_env,
            role: Role::Worker,
            env: &ctx,
        },
    ) {
        Ok(launch) => launch,
        Err(error) => panic!("{id} should launch: {error}"),
    };
    let prompt = match launch.brief {
        BriefRoute::InArgs => KICKOFF,
        BriefRoute::Typed => BRIEF,
    };
    let argv = one_shot(&harness, &launch.argv, prompt);
    let (status, stdout, stderr) = run(&argv, dir.path());
    let reply = format!("{stdout}\n{stderr}").to_lowercase();
    // A spent account is not a broken harness: the program says so in its own
    // words, and the test says it skipped instead of failing.
    if let Some(phrase) = exhausted(&harness.spec().dry_patterns, &reply) {
        println!("skipped: {id} out of quota (matched `{phrase}`)");
        return;
    }
    assert!(
        status.success(),
        "{id} exited with {status}\nargv: {argv:?}\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stdout.trim().is_empty(),
        "{id} answered nothing\nargv: {argv:?}\nstderr: {stderr}"
    );
    assert!(
        reply.contains("banana"),
        "{id} did not read the brief\nargv: {argv:?}\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
#[ignore = "starts a real Claude Code session and costs one reply"]
fn claude_answers_a_brief() {
    live("claude");
}

#[test]
#[ignore = "starts a real Codex session and costs one reply"]
fn codex_answers_a_brief() {
    live("codex");
}

#[test]
#[ignore = "starts a real pi session and costs one reply"]
fn pi_answers_a_brief() {
    live("pi");
}

#[test]
#[ignore = "starts a real OpenCode session and costs one reply"]
fn opencode_answers_a_brief() {
    live("opencode");
}

#[test]
#[ignore = "starts a real kilo session and costs one reply"]
fn kilo_answers_a_brief() {
    live("kilo");
}
