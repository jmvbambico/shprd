//! End-to-end tests for `shprd doctor`: fake CLIs on a private `PATH`, a temp
//! `HOME` holding skills, a temp copy of `data/`, and `--no-quota` so no probe
//! touches the network.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

mod support;

use support::herdr::isolate;

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
///
/// Written beside its final path and renamed into place, so the path the test
/// then executes is never open for writing: a sibling thread's child cannot
/// inherit a write descriptor on it and make the exec fail with `ETXTBSY`.
fn write_script(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    let temp = dir.join(format!("{name}.tmp"));
    if let Err(error) = fs::write(&temp, format!("#!/bin/sh\n{body}\n")) {
        panic!("writing {name} failed: {error}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = fs::set_permissions(&temp, fs::Permissions::from_mode(0o755)) {
            panic!("chmod {name} failed: {error}");
        }
    }
    if let Err(error) = fs::rename(&temp, &path) {
        panic!("renaming {name} into place failed: {error}");
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
        // An ignored folder with a skill inside it: junk, reported once.
        write_skill(&skills.join(".trash"), "old");
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

    /// The same machine, with the roles in `settings.toml` and no `config.toml`
    /// at all: the M9 install the engine actually reads.
    fn settings_only(roles: &str) -> Self {
        let setup = Self::new("");
        let config = setup.tmp.path().join("home").join(".config").join("shprd");
        // A roster line with nothing in it is still a file the legacy layer
        // reads, so it is removed rather than left empty.
        let _ = fs::remove_file(config.join("config.toml"));
        if let Err(error) = fs::write(config.join("settings.toml"), roles) {
            panic!("writing the settings failed: {error}");
        }
        setup
    }

    /// Runs `shprd doctor --no-quota` and returns what it printed.
    fn run(&self, env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(BIN);
        // The temp home, not the machine: a diagnosis of this fixture, and a
        // child that cannot reach the person's herdr session or desktop.
        isolate(&mut command, &self.tmp.path().join("home"));
        command
            .args(["doctor", "--no-quota"])
            .env("PATH", self.tmp.path().join("bin").display().to_string())
            .env("HOME", self.tmp.path().join("home").display().to_string())
            // The temp home's own XDG directories, so the diagnosis is of this
            // fixture and not of whatever the machine has installed.
            .env(
                "XDG_CONFIG_HOME",
                self.tmp.path().join("home/.config").display().to_string(),
            )
            .env(
                "XDG_STATE_HOME",
                self.tmp
                    .path()
                    .join("home/.local/state")
                    .display()
                    .to_string(),
            )
            .env(
                "XDG_DATA_HOME",
                self.tmp
                    .path()
                    .join("home/.local/share")
                    .display()
                    .to_string(),
            )
            .env(
                "SHPRD_DATA_DIR",
                self.tmp.path().join("data").display().to_string(),
            )
            .env_remove("SHPRD_HOME")
            .env_remove("SHPRD_PI_MODEL")
            // `TUIOS_ENV`, `HERDR_ENV` and every other host variable are
            // removed by `isolate` above, so a doctor run from a herdr pane
            // diagnoses the fixture and not the pane it started in.
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
    // A pi reviewer on DeepSeek reviews a Claude worker: different model
    // families, so the section passes.
    let setup = Setup::new(
        "orchestrator = \"claude/opus\"\nreviewer = \"pi/openrouter/deepseek/deepseek-v4.1-flash\"\nworkers = [\"claude/opus\"]",
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
    let junk: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("junk folder"))
        .collect();
    assert!(
        !junk.is_empty(),
        "the junk folder should be reported: {text}"
    );
    for line in junk {
        assert!(
            line.trim_end().ends_with("~/.agents/skills/.trash"),
            "a junk line names the folder itself: {line}"
        );
    }
    assert!(
        !text.contains(".trash/"),
        "no file inside a junk folder may be listed: {text}"
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
    assert!(
        fail.contains("runs the anthropic family: the same as worker(s)"),
        "wrong failure: {fail}"
    );
    assert!(
        fail.contains("claude"),
        "the reviewer should be named: {fail}"
    );
}

/// A home holding only `settings.toml` is the M9 install, and the page must
/// judge the roles the engine dispatches from: the reviewer it finds there, and
/// the cross-vendor pair it makes with the worker.
#[test]
fn reads_the_roles_from_settings_when_there_is_no_legacy_config() {
    let setup = Setup::settings_only(
        "version = 1\n\n\
         [roles.worker]\nagent = \"pi\"\nmodel = \"commandcode/deepseek/deepseek-v4.1-flash\"\n\n\
         [roles.reviewer]\nagent = \"pi\"\nmodel = \"commandcode/z-ai/glm-5.3-flash\"\n",
    );
    let output = setup.run(&[]);
    let text = stdout(&output);
    assert!(output.status.success(), "doctor should pass: {text}");
    assert!(
        text.contains("✓ reviewer pi/commandcode/z-ai/glm-5.3-flash (you)"),
        "the reviewer comes from the settings: {text}"
    );
    assert!(
        text.contains(
            "✓ reviewer pi/commandcode/z-ai/glm-5.3-flash runs the zhipu family, independent of every worker"
        ),
        "the cross-vendor pair is judged: {text}"
    );
    assert!(
        text.contains("✓ workers pi/commandcode/deepseek/deepseek-v4.1-flash"),
        "the worker role is the first a spawn tries: {text}"
    );
    assert!(
        !text.contains("reviewer not set"),
        "a configured reviewer is never called unset: {text}"
    );
    assert!(
        !text.contains("workers not set"),
        "a configured worker is never called unset: {text}"
    );
}

/// The Config line names only files that are there: a settings-only install has
/// no `config.toml`, and naming two paths that do not exist sends a person
/// looking for a file shprd never writes (item 5 of the M10.0 follow-ups).
#[test]
fn the_config_line_names_only_files_that_exist() {
    let setup = Setup::settings_only(
        "version = 1\n\n\
         [roles.worker]\nagent = \"pi\"\nmodel = \"commandcode/deepseek/deepseek-v4.1-flash\"\n\n\
         [roles.reviewer]\nagent = \"pi\"\nmodel = \"commandcode/z-ai/glm-5.3-flash\"\n",
    );
    let output = setup.run(&[]);
    let text = stdout(&output);
    assert!(output.status.success(), "doctor should pass: {text}");
    let config_line = text
        .lines()
        .find(|line| line.contains("loads and validates"))
        .unwrap_or_else(|| panic!("no Config line in {text}"));
    assert!(
        !config_line.contains(".config/shprd/config.toml")
            && !config_line.contains(".shprd/config.toml"),
        "no path to a file that is not there is given: {config_line}"
    );
    assert!(
        config_line.contains("no config.toml: the settings file is the only layer"),
        "the page says which layer answered: {config_line}"
    );
}

/// With both config files there, both are named: that is the shape the two-path
/// line is for.
#[test]
fn the_config_line_names_both_files_when_both_are_there() {
    let setup = Setup::new(
        "orchestrator = \"claude/opus\"\nreviewer = \"pi/openrouter/deepseek/deepseek-v4.1-flash\"\nworkers = [\"claude/opus\"]",
    );
    // A repository layer too, which the run is pointed at.
    let root = setup.tmp.path().join("repo");
    let repo_config = root.join(".shprd");
    if let Err(error) = fs::create_dir_all(&repo_config) {
        panic!("creating {} failed: {error}", repo_config.display());
    }
    if let Err(error) = fs::write(repo_config.join("config.toml"), "[gates]\n") {
        panic!("writing the repo config failed: {error}");
    }
    let output = setup.run(&[
        ("SHPRD_REPO", &root.display().to_string()),
        (
            "SHPRD_HOME",
            &setup.tmp.path().join("home").display().to_string(),
        ),
    ]);
    let text = stdout(&output);
    let config_line = text
        .lines()
        .find(|line| line.contains("loads and validates"))
        .unwrap_or_else(|| panic!("no Config line in {text}"));
    assert!(
        config_line.contains(".config/shprd/config.toml"),
        "the person's file is named: {config_line}"
    );
    assert!(
        config_line.contains(".shprd/config.toml"),
        "the repository's file is named too: {config_line}"
    );
    assert!(
        !config_line.contains("no config.toml:"),
        "a page with both files says nothing about a missing one: {config_line}"
    );
}

/// The legacy roster is still the layer under the settings: a home with only
/// `config.toml` keeps the role lines it wrote.
#[test]
fn falls_back_to_the_legacy_roster_when_the_settings_name_no_role() {
    let setup = Setup::settings_only("version = 1\n");
    let config = setup.tmp.path().join("home").join(".config").join("shprd");
    if let Err(error) = fs::write(
        config.join("config.toml"),
        "[roster]\nreviewer = \"pi/openrouter/anthropic/claude-opus-5-5\"\nworkers = [\"claude/opus\"]\n",
    ) {
        panic!("writing the config failed: {error}");
    }
    let output = setup.run(&[]);
    let text = stdout(&output);
    assert!(
        text.contains("✓ reviewer pi/openrouter/anthropic/claude-opus-5-5 (your old config file)"),
        "the roster layer answers when the settings do not: {text}"
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
