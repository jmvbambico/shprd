//! Making a folder a repository shprd can work in, with nobody typing a command.
//!
//! A person who adds a folder to the Herdr window and chats expects the work to
//! begin. If the folder is not a git repository yet, shprd makes it one at the
//! first chat message: `git init` on `main`, shprd's own defaults, and a first
//! commit of what was there. Showing a folder never does this (a window that
//! committed every folder it was pointed at would be dangerous), so the service
//! calls [`ensure_repo`] only from the chat's send path.
//!
//! Every failure is a plain sentence the window can show as it is: it never
//! tells the person to run something.

use std::fs;
use std::path::Path;
use std::process::Command;

/// The branch a repository shprd makes starts on.
const BRANCH: &str = "main";

/// The identity a first commit is made with when git has none configured.
const FALLBACK_NAME: &str = "shprd";
/// The email that goes with [`FALLBACK_NAME`].
const FALLBACK_EMAIL: &str = "shprd@localhost";

/// What making a folder a repository did, for the one line the chat says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Made {
    /// The plain sentence the person reads in the chat.
    pub note: String,
}

/// Whether `dir` is inside a git working tree.
pub fn is_repo(dir: &Path) -> bool {
    git(dir, &["rev-parse", "--git-dir"]).is_ok()
}

/// Whether the repository at `dir` has a commit to base work on.
fn has_commit(dir: &Path) -> bool {
    git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok()
}

/// Makes `dir` a repository with a first commit when it is not one (or has no
/// commit yet), and answers what it did.
///
/// `Ok(None)` is a folder that needed nothing. An error is one plain sentence
/// saying what could not be done and why.
pub fn ensure_repo(dir: &Path) -> Result<Option<Made>, String> {
    let existed = is_repo(dir);
    if existed && has_commit(dir) {
        return Ok(None);
    }
    if !dir.is_dir() {
        return Err(format!(
            "I can't make {} a repository: the folder is not there any more.",
            dir.display()
        ));
    }
    if !existed {
        init(dir)?;
    }
    let gates = detect_gates(dir);
    write_defaults(dir, &gates)?;
    commit_all(dir)?;
    Ok(Some(Made {
        note: note(existed, &gates),
    }))
}

/// `git init` on `main`, falling back for a git that has no `--initial-branch`.
fn init(dir: &Path) -> Result<(), String> {
    let first = git(dir, &["init", "--quiet", "--initial-branch", BRANCH]);
    if first.is_ok() {
        return Ok(());
    }
    let plain = git(dir, &["init", "--quiet"]);
    match plain {
        Ok(_) => {
            let head = format!("refs/heads/{BRANCH}");
            git(dir, &["symbolic-ref", "HEAD", &head])
                .map(|_| ())
                .map_err(|why| failed("make this folder a git repository", &why))
        }
        Err(why) => Err(failed("make this folder a git repository", &why)),
    }
}

/// The commands that check this folder's work, from the files it holds.
///
/// Only what a file names outright is believed: a `Cargo.toml` is `cargo test`, a
/// `package.json` with a real `test` script is `npm test`, a `go.mod` is `go
/// test`, a Python project with tests is `python -m pytest`. An empty list is an
/// honest answer; the chat says so.
pub fn detect_gates(dir: &Path) -> Vec<String> {
    let mut gates = Vec::new();
    if dir.join("Cargo.toml").is_file() {
        gates.push("cargo test".to_string());
    }
    let package = fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    if let Some(json) = package {
        let script = json
            .pointer("/scripts/test")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        // `npm init`'s placeholder is not a test.
        if !script.is_empty() && !script.contains("no test specified") {
            gates.push("npm test".to_string());
        }
    }
    if dir.join("go.mod").is_file() {
        gates.push("go test ./...".to_string());
    }
    let python = dir.join("pyproject.toml").is_file()
        || dir.join("pytest.ini").is_file()
        || dir.join("setup.py").is_file();
    if python && (dir.join("tests").is_dir() || dir.join("test").is_dir()) {
        gates.push("python -m pytest".to_string());
    }
    gates
}

/// Writes shprd's defaults for a new repository: its config (the gates found, or
/// none) and an ignore for its run files. Nothing already there is overwritten.
fn write_defaults(dir: &Path, gates: &[String]) -> Result<(), String> {
    let shprd = dir.join(".shprd");
    fs::create_dir_all(&shprd)
        .map_err(|why| failed("write shprd's settings into this folder", &why.to_string()))?;
    let config = shprd.join("config.toml");
    if !config.exists() {
        let body = if gates.is_empty() {
            "# shprd's settings for this repository. No test command was found when it\n\
             # was made; add one under [gates] to have every task checked by it.\n\
             [gates]\nworker = []\n"
                .to_string()
        } else {
            let list: Vec<String> = gates.iter().map(|gate| format!("  {gate:?},")).collect();
            format!(
                "# shprd's settings for this repository, written when it was made.\n\
                 [gates]\nworker = [\n{}\n]\n",
                list.join("\n")
            )
        };
        fs::write(&config, body)
            .map_err(|why| failed("write shprd's settings into this folder", &why.to_string()))?;
    }
    // The run's own files stay out of the history; only its config is kept.
    let ignore = dir.join(".gitignore");
    let existing = fs::read_to_string(&ignore).unwrap_or_default();
    // Each line is added on its own: a .gitignore that already has the first must
    // still get the exception that keeps the config in the first commit.
    let missing: Vec<&str> = [".shprd/*", "!.shprd/config.toml"]
        .into_iter()
        .filter(|rule| !existing.lines().any(|line| line.trim() == *rule))
        .collect();
    if !missing.is_empty() {
        let mut text = existing;
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        for rule in missing {
            text.push_str(rule);
            text.push('\n');
        }
        fs::write(&ignore, text)
            .map_err(|why| failed("write shprd's settings into this folder", &why.to_string()))?;
    }
    Ok(())
}

/// Commits everything in the folder as the first commit.
fn commit_all(dir: &Path) -> Result<(), String> {
    git(dir, &["add", "--all"]).map_err(|why| failed("add this folder's files to git", &why))?;
    let configured = |key: &str| {
        git(dir, &["config", key])
            .map(|out| !out.trim().is_empty())
            .unwrap_or(false)
    };
    let mut args: Vec<String> = Vec::new();
    if !configured("user.name") {
        args.push("-c".into());
        args.push(format!("user.name={FALLBACK_NAME}"));
    }
    if !configured("user.email") {
        args.push("-c".into());
        args.push(format!("user.email={FALLBACK_EMAIL}"));
    }
    // A first commit must not wait for a signing key nobody is there to unlock.
    args.extend(["-c".into(), "commit.gpgsign=false".into()]);
    args.extend(
        [
            "commit",
            "--quiet",
            "--allow-empty",
            "--no-verify",
            "-m",
            "Start with this folder",
        ]
        .map(String::from),
    );
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    git(dir, &refs)
        .map(|_| ())
        .map_err(|why| failed("make the first commit", &why))
}

/// The one line the chat says after making the repository.
fn note(existed: bool, gates: &[String]) -> String {
    let what = if existed {
        "This repository had no commit yet, so I made a first one"
    } else {
        "This folder wasn't a git repository, so I made it one (branch main) with a first commit of what was here"
    };
    if gates.is_empty() {
        format!(
            "{what}. I found no test command to check work with, so tasks will be checked by review only; tell me if there is one to use."
        )
    } else {
        format!("{what}. Work will be checked with: {}.", gates.join(", "))
    }
}

/// One failure as a sentence: what could not be done, and git's own reason.
fn failed(what: &str, why: &str) -> String {
    let reason = why
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    if reason.is_empty() {
        format!("I couldn't {what}.")
    } else {
        format!("I couldn't {what}: {reason}")
    }
}

/// Runs git in `dir`, answering its stdout, or its stderr as the error.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir).args(args);
    let output = shprd_core::spawn::output(&mut command)
        .map_err(|why| format!("git could not be run ({why})"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        } else {
            stderr
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn out(dir: &Path, args: &[&str]) -> String {
        git(dir, args).expect("git").trim().to_string()
    }

    #[test]
    fn an_empty_folder_becomes_a_repository_on_main_with_a_commit() {
        let temp = tempfile::tempdir().expect("dir");
        let made = ensure_repo(temp.path())
            .expect("made")
            .expect("it did something");
        assert!(made.note.contains("git repository"), "{}", made.note);
        assert!(made.note.contains("no test command"), "{}", made.note);
        assert!(!made.note.contains("run `"), "{}", made.note);
        assert_eq!(
            out(temp.path(), &["rev-parse", "--abbrev-ref", "HEAD"]),
            "main"
        );
        assert!(has_commit(temp.path()));
        assert!(temp.path().join(".shprd/config.toml").is_file());
        let tracked = out(temp.path(), &["ls-files"]);
        assert!(tracked.contains(".gitignore"), "{tracked}");
        assert!(tracked.contains(".shprd/config.toml"), "{tracked}");
    }

    #[test]
    fn a_folder_with_files_commits_them_and_finds_its_gates() {
        let temp = tempfile::tempdir().expect("dir");
        fs::write(temp.path().join("hello.py"), "print('hi')\n").expect("write");
        fs::write(temp.path().join("Cargo.toml"), "[package]\n").expect("write");
        let made = ensure_repo(temp.path()).expect("made").expect("did");
        assert!(made.note.contains("cargo test"), "{}", made.note);
        let tracked = out(temp.path(), &["ls-files"]);
        assert!(tracked.contains("hello.py"), "{tracked}");
        let config = fs::read_to_string(temp.path().join(".shprd/config.toml")).expect("config");
        assert!(config.contains("cargo test"), "{config}");
    }

    #[test]
    fn a_repository_with_a_commit_is_left_alone() {
        let temp = tempfile::tempdir().expect("dir");
        ensure_repo(temp.path()).expect("made");
        let before = out(temp.path(), &["rev-parse", "HEAD"]);
        assert_eq!(ensure_repo(temp.path()).expect("again"), None);
        assert_eq!(out(temp.path(), &["rev-parse", "HEAD"]), before);
    }

    #[test]
    fn a_folder_inside_a_repository_uses_that_repository() {
        let temp = tempfile::tempdir().expect("dir");
        ensure_repo(temp.path()).expect("made");
        let inner = temp.path().join("src");
        fs::create_dir(&inner).expect("mkdir");
        assert_eq!(ensure_repo(&inner).expect("inside"), None);
        assert!(!inner.join(".git").exists());
    }

    #[test]
    fn a_repository_with_no_commit_gets_one_without_a_new_init() {
        let temp = tempfile::tempdir().expect("dir");
        git(temp.path(), &["init", "--quiet", "-b", "trunk"]).expect("init");
        let made = ensure_repo(temp.path()).expect("made").expect("did");
        assert!(made.note.contains("no commit yet"), "{}", made.note);
        assert_eq!(
            out(temp.path(), &["rev-parse", "--abbrev-ref", "HEAD"]),
            "trunk"
        );
    }

    #[test]
    fn an_existing_ignore_with_the_run_rule_still_keeps_the_config() {
        let temp = tempfile::tempdir().expect("dir");
        fs::write(temp.path().join(".gitignore"), ".shprd/*\ntarget\n").expect("write");
        ensure_repo(temp.path()).expect("made");
        let tracked = out(temp.path(), &["ls-files"]);
        assert!(tracked.contains(".shprd/config.toml"), "{tracked}");
    }

    #[test]
    fn a_folder_that_is_not_there_says_so_in_plain_words() {
        let temp = tempfile::tempdir().expect("dir");
        let gone = temp.path().join("gone");
        let why = ensure_repo(&gone).expect_err("no folder");
        assert!(why.contains("not there"), "{why}");
        assert!(!why.contains("shprd "), "names no command: {why}");
    }

    #[test]
    fn an_npm_placeholder_is_not_a_gate() {
        let temp = tempfile::tempdir().expect("dir");
        fs::write(
            temp.path().join("package.json"),
            r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#,
        )
        .expect("write");
        assert!(detect_gates(temp.path()).is_empty());
        fs::write(
            temp.path().join("package.json"),
            r#"{"scripts":{"test":"vitest"}}"#,
        )
        .expect("write");
        assert_eq!(detect_gates(temp.path()), vec!["npm test".to_string()]);
    }
}
