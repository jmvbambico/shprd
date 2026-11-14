//! End-to-end tests for `shprd prompts`: the files written on first use, where
//! they live when `XDG_CONFIG_HOME` is set, an edit that survives, and reset.
//!
//! Every test gets its own home and config directory, so nothing reads or
//! writes the developer's own prompts.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// A temp home and config directory, with the binary run against both.
struct Fixture {
    home: TempDir,
    config: TempDir,
}

impl Fixture {
    /// A fresh home and XDG config directory.
    fn new() -> Fixture {
        Fixture {
            home: TempDir::new().expect("a temp home"),
            config: TempDir::new().expect("a temp config"),
        }
    }

    /// Runs `shprd prompts <args>` with `XDG_CONFIG_HOME` pointing at the
    /// fixture.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .arg("prompts")
            .args(args)
            .env("SHPRD_HOME", self.home.path())
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.config.path())
            .output()
            .unwrap_or_else(|error| panic!("running {BIN} failed: {error}"))
    }

    /// Runs a verb and returns its stdout, asserting it succeeded.
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "`prompts {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("utf-8 output")
    }

    /// Runs a verb as JSON and parses its stdout.
    fn json(&self, args: &[&str]) -> Value {
        let text = self.ok(args);
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    /// The prompts directory the fixture should use.
    fn prompts_dir(&self) -> PathBuf {
        self.config.path().join("shprd").join("prompts")
    }

    /// One prompt's file in the fixture's directory.
    fn file(&self, id: &str) -> PathBuf {
        self.prompts_dir().join(format!("{id}.md"))
    }
}

#[test]
fn the_first_list_writes_every_shipped_prompt() {
    let fixture = Fixture::new();
    let text = fixture.ok(&["list"]);
    for id in ["assistant", "worker", "reviewer", "backup"] {
        assert!(
            text.contains(&format!("{id} ")),
            "the list names {id}: {text}"
        );
        assert!(
            fixture.file(id).exists(),
            "listing writes {id}.md on first use"
        );
    }
    assert!(
        text.contains("default"),
        "a fresh file is the default: {text}"
    );
}

#[test]
fn the_prompts_follow_the_xdg_config_home() {
    let fixture = Fixture::new();
    fixture.ok(&["path", "worker"]);
    assert!(fixture.file("worker").exists());
    assert!(
        !fixture.home.path().join(".config").exists(),
        "with XDG_CONFIG_HOME set, nothing is written under the home directory"
    );
}

#[test]
fn without_xdg_the_prompts_live_under_the_home_directory() {
    let home = TempDir::new().expect("a temp home");
    let output = Command::new(BIN)
        .args(["prompts", "path", "worker"])
        .env("SHPRD_HOME", home.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .output()
        .expect("the binary runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let path = String::from_utf8(output.stdout).expect("utf-8 output");
    assert_eq!(
        path.trim(),
        home.path()
            .join(".config/shprd/prompts/worker.md")
            .display()
            .to_string()
    );
}

#[test]
fn show_prints_the_shipped_words_and_path_names_the_file() {
    let fixture = Fixture::new();
    let words = fixture.ok(&["show", "worker"]);
    assert!(words.contains("shprd task status"), "{words}");
    assert!(
        words.contains("eliasstravik/shepherdr"),
        "the credit travels"
    );
    let path = fixture.ok(&["path", "worker"]);
    assert_eq!(path.trim(), fixture.file("worker").display().to_string());
}

#[test]
fn an_edit_survives_and_reset_puts_the_shipped_words_back() {
    let fixture = Fixture::new();
    let shipped = fixture.ok(&["show", "worker"]);
    fs::write(fixture.file("worker"), "my own words\n").expect("an edit");

    assert_eq!(fixture.ok(&["show", "worker"]), "my own words\n");
    let listed = fixture.json(&["list", "--json"]);
    let worker = listed["prompts"]
        .as_array()
        .expect("a list of prompts")
        .iter()
        .find(|prompt| prompt["id"] == "worker")
        .expect("the worker prompt");
    assert_eq!(worker["state"], "edited");

    let reset = fixture.json(&["reset", "worker", "--json"]);
    assert_eq!(reset["state"], "default");
    assert_eq!(reset["change"], "reset");
    assert_eq!(fixture.ok(&["show", "worker"]), shipped);
}

#[test]
fn a_prompt_of_your_own_is_read_and_a_missing_one_is_a_refusal() {
    let fixture = Fixture::new();
    fixture.ok(&["list"]);
    fs::write(fixture.file("docs"), "write the docs\n").expect("a custom prompt");
    assert_eq!(fixture.ok(&["show", "docs"]), "write the docs\n");

    let missing = fixture.run(&["show", "nope"]);
    assert_eq!(missing.status.code(), Some(2), "a refusal exits 2");
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(stderr.contains("no prompt `nope`"), "{stderr}");
    assert!(
        stderr.contains("nope.md"),
        "the refusal names the file: {stderr}"
    );
}

#[test]
fn the_coordinator_can_be_asked_for_by_its_role_id() {
    let fixture = Fixture::new();
    let by_role = fixture.ok(&["path", "coordinator"]);
    assert!(
        by_role.trim().ends_with("assistant.md"),
        "the coordinator is the assistant prompt: {by_role}"
    );
    assert_eq!(by_role, fixture.ok(&["path", "assistant"]));
}
