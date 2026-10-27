//! End-to-end tests for `shprd doctor`: fake CLIs on a private `PATH`, a temp
//! `HOME` holding skills, a temp copy of `data/`, and `--no-quota` so no probe
//! touches the network.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// The repo's `data/` directory, copied into each test's tempdir.
fn data_source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .canonicalize()
        .unwrap_or_else(|_| panic!("the data directory should exist"))
}

/// Copies a directory tree, so a test never writes into the real `data/`.
fn copy_tree(from: &Path, to: &Path) {
    if let Err(error) = fs::create_dir_all(to) {
        panic!("creating {} failed: {error}", to.display());
    }
    let Ok(entries) = fs::read_dir(from) else {
        panic!("reading {} failed", from.display());
    };
    for entry in entries.flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else if let Err(error) = fs::copy(entry.path(), &target) {
            panic!("copying to {} failed: {error}", target.display());
        }
    }
}

/// A shell script standing in for a real agent CLI.
fn write_script(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    if let Err(error) = fs::write(&path, format!("#!/bin/sh\n{body}\n")) {
        panic!("writing {name} failed: {error}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(0o755)) {
            panic!("chmod {name} failed: {error}");
        }
    }
}

/// A skill directory with the `SKILL.md` the scanner needs.
fn write_skill(dir: &Path, name: &str) {
    let skill = dir.join(name);
    if let Err(error) = fs::create_dir_all(&skill) {
        panic!("creating {} failed: {error}", skill.display());
    }
    let body = format!("---\nname: {name}\ndescription: a fake skill\n---\n");
    if let Err(error) = fs::write(skill.join("SKILL.md"), body) {
        panic!("writing the skill failed: {error}");
    }
}

/// A machine to diagnose: fake CLIs, a temp home with skills, copied data.
struct Setup {
    /// The temp directory everything lives under.
    tmp: TempDir,
}

impl Setup {
    /// Builds the temp machine with the given `[roster]` config body.
    fn new(roster: &str) -> Self {
        let tmp = match TempDir::new() {
            Ok(tmp) => tmp,
            Err(error) => panic!("tempdir failed: {error}"),
        };
        let data = tmp.path().join("data");
        copy_tree(&data_source(), &data);
        let bin = tmp.path().join("bin");
        if let Err(error) = fs::create_dir_all(&bin) {
            panic!("creating {} failed: {error}", bin.display());
        }
        for name in ["claude", "codex", "opencode", "pi"] {
            write_script(
                &bin,
                name,
                "case \"$1\" in\n  --version) echo '9.9.9 (fake)' ;;\n  *) exit 0 ;;\nesac",
            );
        }
        let home = tmp.path().join("home");
        let skills = home.join(".agents").join("skills");
        write_skill(&skills, "alpha");
        write_skill(&skills, "beta");
        let lock = home.join(".agents").join(".skill-lock.json");
        let body = "{\"skills\":{\"alpha\":{\"source\":\"x/y\",\"skillFolderHash\":\"abc\"}}}";
        if let Err(error) = fs::write(&lock, body) {
            panic!("writing the lock file failed: {error}");
        }
        let config = home.join(".config").join("shprd");
        if let Err(error) = fs::create_dir_all(&config) {
            panic!("creating {} failed: {error}", config.display());
        }
        let layer = format!("[roster]\n{roster}\n");
        if let Err(error) = fs::write(config.join("config.toml"), layer) {
            panic!("writing the config failed: {error}");
        }
        Self { tmp }
    }

    /// Runs `shprd doctor --no-quota` and returns what it printed.
    fn run(&self, env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(BIN);
        command
            .args(["doctor", "--no-quota"])
            .env("PATH", self.tmp.path().join("bin").display().to_string())
            .env("HOME", self.tmp.path().join("home").display().to_string())
            .env(
                "SHPRD_DATA_DIR",
                self.tmp.path().join("data").display().to_string(),
            )
            .env_remove("SHPRD_HOME")
            .env_remove("SHPRD_PI_MODEL")
            .env_remove("TUIOS_ENV")
            .env_remove("HERDR_ENV")
            .env_remove("OPENCODE_DISABLE_EXTERNAL_SKILLS");
        for (name, value) in env {
            command.env(name, value);
        }
        match command.output() {
            Ok(output) => output,
            Err(error) => panic!("running {BIN} failed: {error}"),
        }
    }
}

/// The stdout of a finished run.
fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[test]
fn reports_every_section_and_succeeds_on_a_clean_setup() {
    let setup = Setup::new(
        "orchestrator = \"claude/opus\"\nreviewer = \"pi/anthropic/claude-sonnet\"\nworkers = [\"claude/opus\"]",
    );
    let output = setup.run(&[]);
    let text = stdout(&output);
    assert!(output.status.success(), "doctor should pass: {text}");
    for title in ["Host", "Data", "Config", "Agents", "Review", "Skills"] {
        assert!(
            text.lines().any(|line| line == title),
            "no {title} section in {text}"
        );
    }
    assert!(text.contains("Launch env"), "no launch env section: {text}");
    assert!(!text.contains('✗'), "no line should fail: {text}");
    assert!(
        !text.lines().any(|line| line == "Quota"),
        "--no-quota should skip the quota section: {text}"
    );
    assert!(
        text.contains("2 skill(s) visible"),
        "the fake skills should be counted: {text}"
    );
    let unknown = text
        .lines()
        .find(|line| line.contains("unknown (not installed"))
        .unwrap_or_else(|| panic!("no unknown sign-in in {text}"));
    assert!(
        unknown.trim_start().starts_with('!'),
        "an unknown sign-in must warn, not pass: {unknown}"
    );
}

#[test]
fn fails_when_the_reviewer_shares_a_vendor_with_a_worker() {
    let setup = Setup::new(
        "orchestrator = \"claude/opus\"\nreviewer = \"claude/sonnet\"\nworkers = [\"claude/opus\"]",
    );
    let output = setup.run(&[]);
    let text = stdout(&output);
    assert_eq!(output.status.code(), Some(1), "doctor should fail: {text}");
    let fail = text
        .lines()
        .find(|line| line.trim_start().starts_with("✗"))
        .unwrap_or_else(|| panic!("no failing line in {text}"));
    assert!(fail.contains("same vendor"), "wrong failure: {fail}");
    assert!(
        fail.contains("claude"),
        "the reviewer should be named: {fail}"
    );
}

#[test]
fn warns_when_this_shell_hides_a_skill_directory() {
    let setup = Setup::new(
        "orchestrator = \"claude/opus\"\nreviewer = \"pi/anthropic/claude-sonnet\"\nworkers = [\"claude/opus\"]",
    );
    let var = "OPENCODE_DISABLE_EXTERNAL_SKILLS";
    let output = setup.run(&[(var, "1")]);
    let text = stdout(&output);
    let warn = text
        .lines()
        .find(|line| line.trim_start().starts_with('!') && line.contains(var))
        .unwrap_or_else(|| panic!("no warning naming {var} in {text}"));
    assert!(
        warn.contains("~/.agents/skills"),
        "the hidden directory should be named: {warn}"
    );
}
