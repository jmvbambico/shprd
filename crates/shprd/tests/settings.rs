//! End-to-end tests for `shprd settings`: a temp home, a temp repository and the
//! shipped `data/` directory, driven through the binary.
//!
//! Every test gets its own home and repository, so nothing reads or writes the
//! developer's own settings, and the harness table is the one shprd ships, so
//! the role checks are the real ones.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// A temp home and repository, with the binary run against both.
///
/// The repository is a folder named `shprd` inside a temp directory, because
/// the settings key a repository's entry by its folder's name.
struct Fixture {
    home: TempDir,
    #[allow(dead_code)]
    root: TempDir,
    repo: PathBuf,
}

impl Fixture {
    /// A fresh home and repository, neither holding a settings file.
    fn new() -> Fixture {
        let root = TempDir::new().expect("a temp root");
        let repo = root.path().join("shprd");
        fs::create_dir_all(repo.join(".shprd")).expect("the repo directory");
        Fixture {
            home: TempDir::new().expect("a temp home"),
            root,
            repo,
        }
    }

    /// Runs `shprd settings <args>` for this fixture.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .arg("settings")
            .args(args)
            .arg("--repo")
            .arg(&self.repo)
            .env("SHPRD_HOME", self.home.path())
            .env("SHPRD_DATA_DIR", data_dir())
            .output()
            .unwrap_or_else(|error| panic!("running {BIN} failed: {error}"))
    }

    /// Runs `shprd settings <args>` with no data directory anywhere: the
    /// environment's is removed and the temp home holds none, so the role
    /// checks have nothing to read.
    fn run_without_data(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .arg("settings")
            .args(args)
            .arg("--repo")
            .arg(&self.repo)
            .env_remove("SHPRD_DATA_DIR")
            .env_remove("XDG_DATA_HOME")
            .env("SHPRD_HOME", self.home.path())
            .env("HOME", self.home.path())
            .output()
            .unwrap_or_else(|error| panic!("running {BIN} failed: {error}"))
    }

    /// Runs any shprd verb in this fixture's repository.
    fn shprd(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .arg("--repo")
            .arg(&self.repo)
            .arg("--no-dagr")
            .env("SHPRD_HOME", self.home.path())
            .env("SHPRD_DATA_DIR", data_dir())
            .output()
            .unwrap_or_else(|error| panic!("running {BIN} failed: {error}"))
    }

    /// Runs a verb and returns its stdout, asserting it succeeded.
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "`settings {}` should succeed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Runs a verb and returns its stderr, asserting it was refused with exit 2.
    fn refused(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "`settings {}` should be refused: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout)
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    /// The person's settings file.
    fn settings(&self) -> PathBuf {
        self.home.path().join(".config/shprd/settings.toml")
    }

    /// The person's pre-M9 config file.
    fn config(&self) -> PathBuf {
        self.home.path().join(".config/shprd/config.toml")
    }

    /// The repository's committed settings file.
    fn committed(&self) -> PathBuf {
        self.repo.join(".shprd/settings.toml")
    }

    /// Writes one of the files above, creating its directory.
    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("the file's directory");
        }
        fs::write(path, text).expect("the file is written");
    }

    /// The text of one of the files above, empty when it does not exist.
    fn text(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }
}

/// The shipped `data/` directory: the real harness and family tables.
fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .canonicalize()
        .expect("the shipped data directory")
}

/// The JSON a verb printed with `--json`.
fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("not JSON ({error}): {text}"))
}

/// One key's value and source from a `show --json` document.
fn row<'a>(document: &'a Value, key: &str) -> &'a Value {
    document["values"]
        .as_array()
        .expect("values is a list")
        .iter()
        .find(|row| row["key"] == key)
        .unwrap_or_else(|| panic!("no row `{key}`"))
}

#[test]
fn show_prints_every_key_with_the_layer_it_came_from() {
    let fixture = Fixture::new();
    let shown = fixture.ok(&["show"]);
    assert!(
        shown.contains("tasks.merge = \"ask\"  (built in)"),
        "{shown}"
    );
    assert!(
        shown.contains("roles.coordinator.title = \"Assistant\"  (built in)"),
        "{shown}"
    );
    assert!(
        shown.contains("roles.worker.agent = (unset)"),
        "a pick nobody made is offered, not hidden: {shown}"
    );
    assert!(
        shown.contains("review.cross_vendor = true  (built in)"),
        "{shown}"
    );
}

#[test]
fn show_json_lists_every_value_with_its_source() {
    let fixture = Fixture::new();
    let document = json(&fixture.ok(&["show", "--json"]));
    assert!(document["repo"].is_string(), "the repo is named");
    assert_eq!(row(&document, "tasks.merge")["value"], "ask");
    assert_eq!(row(&document, "tasks.merge")["source"], "built_in");
    assert_eq!(row(&document, "roles.worker.agent")["value"], Value::Null);
    assert_eq!(row(&document, "roles.worker.agent")["source"], Value::Null);
    assert_eq!(document["problems"], serde_json::json!([]));
}

#[test]
fn set_stores_a_value_and_get_reads_it_back() {
    let fixture = Fixture::new();
    let printed = fixture.ok(&["set", "tasks.merge", "checks"]);
    assert!(printed.contains("ok tasks.merge stored"), "{printed}");
    assert!(printed.contains("(you)"), "{printed}");
    let text = Fixture::text(&fixture.settings());
    assert!(text.contains("version = 1"), "{text}");
    assert!(text.contains("merge = \"checks\""), "{text}");

    let shown = fixture.ok(&["get", "tasks.merge"]);
    assert_eq!(shown.trim(), "tasks.merge = \"checks\"  (you)");

    // The value is a real setting: the typed accessor sees it too.
    let document = json(&fixture.ok(&["show", "--json"]));
    assert_eq!(row(&document, "tasks.merge")["source"], "you");
}

#[test]
fn set_for_repo_writes_that_repositorys_entry() {
    let fixture = Fixture::new();
    let name = "shprd";
    let printed = fixture.ok(&["set", "tasks.merge", "auto", "--for-repo"]);
    assert!(printed.contains("(this repo)"), "{printed}");
    let text = Fixture::text(&fixture.settings());
    assert!(text.contains(&format!("[repos.{name}.tasks]")), "{text}");
    assert!(text.contains("merge = \"auto\""), "{text}");

    let document = json(&fixture.ok(&["show", "--json"]));
    assert_eq!(row(&document, "tasks.merge")["source"], "repo_entry");

    // Removing it leaves the person's own value in place.
    fixture.ok(&["set", "tasks.merge", "checks"]);
    let printed = fixture.ok(&["unset", "tasks.merge", "--for-repo"]);
    assert!(printed.contains("ok tasks.merge unset"), "{printed}");
    let document = json(&fixture.ok(&["show", "--json"]));
    assert_eq!(row(&document, "tasks.merge")["source"], "you");
}

#[test]
fn a_value_equal_to_the_inherited_one_is_not_stored() {
    let fixture = Fixture::new();
    fixture.ok(&["set", "tasks.merge", "checks"]);
    let printed = fixture.ok(&["set", "tasks.merge", "ask"]);
    assert!(
        printed.contains("not stored: it equals what this scope inherits"),
        "{printed}"
    );
    let text = Fixture::text(&fixture.settings());
    assert!(
        !text.contains("merge"),
        "the equal value is not stored: {text}"
    );
    let shown = fixture.ok(&["get", "tasks.merge"]);
    assert_eq!(shown.trim(), "tasks.merge = \"ask\"  (built in)");
}

#[test]
fn unset_removes_the_key_and_says_so() {
    let fixture = Fixture::new();
    let printed = fixture.ok(&["unset", "tasks.max_running"]);
    assert!(printed.contains("was already unset"), "{printed}");
    assert!(
        !fixture.settings().exists(),
        "an unset of nothing writes nothing"
    );

    fixture.ok(&["set", "tasks.max_running", "2"]);
    let printed = fixture.ok(&["unset", "tasks.max_running"]);
    assert!(printed.contains("ok tasks.max_running unset"), "{printed}");
    let text = Fixture::text(&fixture.settings());
    assert!(!text.contains("max_running"), "{text}");
}

#[test]
fn the_repo_committed_file_is_read_and_may_only_set_its_allow_list() {
    let fixture = Fixture::new();
    Fixture::write(
        &fixture.committed(),
        "version = 1\n[tasks]\nbase = \"trunk\"\n",
    );
    let shown = fixture.ok(&["get", "tasks.base"]);
    assert_eq!(shown.trim(), "tasks.base = \"trunk\"  (the repo's file)");

    for document in [
        "version = 1\n[tasks]\nmerge = \"auto\"\n",
        "version = 1\n[roles.worker]\nagent = \"codex\"\n",
        "version = 1\n[review]\ncross_vendor = false\n",
    ] {
        Fixture::write(&fixture.committed(), document);
        let refused = fixture.refused(&["get", "tasks.base"]);
        assert!(
            refused.contains("may only set `tasks.base`"),
            "{document} should be refused: {refused}"
        );
        assert!(
            refused.contains("widen its own guards"),
            "the refusal says why: {refused}"
        );
    }
}

#[test]
fn the_old_config_keys_still_load() {
    let fixture = Fixture::new();
    Fixture::write(
        &fixture.config(),
        "[tick]\nmax_running = 2\n\n[janitor]\narchive_days = 30\n\n[land]\nmerge = \"checks\"\n",
    );
    assert_eq!(
        fixture.ok(&["get", "tasks.max_running"]).trim(),
        "tasks.max_running = 2  (your old config file)"
    );
    assert_eq!(
        fixture.ok(&["get", "tasks.archive_after"]).trim(),
        "tasks.archive_after = 30  (your old config file)"
    );
    assert_eq!(
        fixture.ok(&["get", "tasks.merge"]).trim(),
        "tasks.merge = \"checks\"  (your old config file)"
    );
    // The old names are accepted as keys, and reported under their new name.
    assert_eq!(
        fixture.ok(&["get", "tick.max_running"]).trim(),
        "tasks.max_running = 2  (your old config file)"
    );
    let document = json(&fixture.ok(&["show", "--json"]));
    assert_eq!(row(&document, "tasks.merge")["source"], "legacy");
}

#[test]
fn the_old_roster_reviewer_shows_as_the_legacy_layer() {
    // The pre-M9 way of naming a reviewer still resolves, and `settings show`
    // says where it came from instead of pretending the person set it here.
    let fixture = Fixture::new();
    Fixture::write(
        &fixture.config(),
        "[roster]\nreviewer = \"codex/gpt-5-codex\"\n\n[roster.effort]\n\"codex/gpt-5-codex\" = \"high\"\n",
    );
    assert_eq!(
        fixture.ok(&["get", "roles.reviewer.agent"]).trim(),
        "roles.reviewer.agent = \"codex\"  (your old config file)"
    );
    assert_eq!(
        fixture.ok(&["get", "roles.reviewer.model"]).trim(),
        "roles.reviewer.model = \"gpt-5-codex\"  (your old config file)"
    );
    assert_eq!(
        fixture.ok(&["get", "roles.reviewer.effort"]).trim(),
        "roles.reviewer.effort = \"high\"  (your old config file)"
    );
    let document = json(&fixture.ok(&["show", "--json"]));
    assert_eq!(row(&document, "roles.reviewer.agent")["source"], "legacy");

    // A value set the new way wins and reads as the person's own.
    fixture.ok(&["set", "roles.reviewer.model", "gpt-6"]);
    let document = json(&fixture.ok(&["show", "--json"]));
    assert_eq!(row(&document, "roles.reviewer.agent")["source"], "legacy");
    assert_eq!(row(&document, "roles.reviewer.model")["source"], "you");
}

#[test]
fn a_same_vendor_review_is_refused_and_nothing_is_written() {
    let fixture = Fixture::new();
    fixture.ok(&["set", "roles.worker.agent", "claude"]);
    let before = Fixture::text(&fixture.settings());

    let refused = fixture.refused(&["set", "roles.reviewer.agent", "claude"]);
    assert!(
        refused.contains("the reviewer `reviewer` and the worker `worker` are both anthropic"),
        "{refused}"
    );
    assert_eq!(
        Fixture::text(&fixture.settings()),
        before,
        "a refused write leaves the file alone"
    );

    // Another vendor is accepted, and the pair checks out.
    let printed = fixture.ok(&["set", "roles.reviewer.agent", "codex"]);
    assert!(
        printed.contains("ok roles.reviewer.agent stored"),
        "{printed}"
    );
    let document = json(&fixture.ok(&["show", "--json"]));
    assert_eq!(document["problems"], serde_json::json!([]));
}

#[test]
fn only_a_review_role_may_review() {
    let fixture = Fixture::new();
    // The built-in reviewer reviews because it only reads; taking both away
    // leaves nothing that may review, and the engine refuses the pair.
    fixture.ok(&["set", "roles.reviewer.readonly", "false"]);
    let refused = fixture.refused(&["set", "roles.reviewer.kind", "worker"]);
    assert!(
        refused.contains("role `reviewer` may not review"),
        "{refused}"
    );
    assert!(
        refused.contains("only a role of kind `reviewer`"),
        "the refusal says who may: {refused}"
    );
    assert!(
        !Fixture::text(&fixture.settings()).contains("kind = \"worker\""),
        "nothing was written"
    );
}

#[test]
fn an_unknown_program_or_mode_or_effort_is_refused() {
    let fixture = Fixture::new();
    let refused = fixture.refused(&["set", "roles.docs.agent", "gemini"]);
    assert!(refused.contains("which no harness defines"), "{refused}");
    assert!(refused.contains("known programs are"), "{refused}");

    // A pick is only checked once it names the program it is for.
    fixture.ok(&["set", "roles.docs.agent", "codex"]);
    fixture.ok(&["set", "roles.docs.perm", "read-only"]);
    let refused = fixture.refused(&["set", "roles.docs.perm", "bypassPermissions"]);
    assert!(
        refused.contains("does not offer"),
        "claude's mode is not codex's: {refused}"
    );
    let refused = fixture.refused(&["set", "roles.docs.effort", "turbo"]);
    assert!(refused.contains("does not list"), "{refused}");
}

#[test]
fn a_bad_value_is_refused_before_anything_is_written() {
    let fixture = Fixture::new();
    let refused = fixture.refused(&["set", "tasks.max_running", "99"]);
    assert!(refused.contains("takes 1 to 10"), "{refused}");
    let refused = fixture.refused(&["set", "tasks.merge", "sometimes"]);
    assert!(refused.contains("ask, checks, auto"), "{refused}");
    let refused = fixture.refused(&["set", "roles.docs.bogus", "x"]);
    assert!(refused.contains("roles.docs.bogus"), "{refused}");
    assert!(!fixture.settings().exists(), "nothing was written");
}

#[test]
fn a_write_aimed_at_a_task_is_refused_with_the_reason() {
    let fixture = Fixture::new();
    let refused = fixture.refused(&["set", "tasks.merge", "checks", "--task", "M9.3"]);
    assert!(
        refused.contains("a task's own settings cannot be written from here"),
        "{refused}"
    );
    assert!(refused.contains("`--task M9.3`"), "{refused}");
    assert!(!fixture.settings().exists());
}

#[test]
fn show_warns_about_settings_that_would_not_work() {
    let fixture = Fixture::new();
    // A file written by hand can hold a pair the verb would refuse, so reading
    // it prints the values and the warning rather than refusing to read.
    Fixture::write(
        &fixture.settings(),
        "[roles.worker]\nagent = \"claude\"\n\n[roles.reviewer]\nagent = \"claude\"\n",
    );
    let output = fixture.run(&["show"]);
    assert!(output.status.success(), "reading never refuses");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stdout.contains("tasks.merge"), "{stdout}");
    assert!(stderr.contains("warning:"), "{stderr}");
    assert!(stderr.contains("both anthropic"), "{stderr}");

    let document = json(&fixture.ok(&["show", "--json"]));
    let problems = document["problems"].as_array().expect("problems is a list");
    assert_eq!(problems.len(), 1, "{document}");
    assert!(
        problems[0]
            .as_str()
            .unwrap_or_default()
            .contains("anthropic")
    );
}

#[test]
fn a_key_nobody_knows_is_refused_with_the_keys_that_exist() {
    let fixture = Fixture::new();
    let refused = fixture.refused(&["get", "tasks.nope"]);
    assert!(
        refused.contains("unknown settings key `tasks.nope`"),
        "{refused}"
    );
    assert!(refused.contains("roles.order"), "{refused}");
}

#[test]
fn a_task_is_the_nearest_layer_and_setting_one_is_refused() {
    let fixture = Fixture::new();
    // A run with one task, whose own attempt ran on Claude Code.
    for args in [
        &[
            "run",
            "start",
            "--run-id",
            "r1",
            "--base",
            "main",
            "--integration",
            "integration/m9",
        ][..],
        &[
            "agent",
            "task",
            "add",
            "--id",
            "T1",
            "--title",
            "t",
            "--scope",
            "a",
            "--acceptance",
            "b",
        ][..],
        &[
            "agent",
            "spawn",
            "--task",
            "T1",
            "--record-only",
            "--attempt",
            "1",
            "--worktree",
            "/tmp/w",
            "--branch",
            "task/T1",
            "--agent",
            "claude/claude-opus-5-5",
        ][..],
    ] {
        let output = fixture.shprd(args);
        assert!(
            output.status.success(),
            "`{}` should succeed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let shown = fixture.ok(&["get", "roles.worker.agent", "--task", "T1"]);
    assert_eq!(shown.trim(), "roles.worker.agent = \"claude\"  (this task)");
    let shown = fixture.ok(&["get", "roles.worker.model", "--task", "T1"]);
    assert_eq!(
        shown.trim(),
        "roles.worker.model = \"claude-opus-5-5\"  (this task)"
    );
    // Every other key still comes from its own layer.
    assert_eq!(
        fixture.ok(&["get", "tasks.merge", "--task", "T1"]).trim(),
        "tasks.merge = \"ask\"  (built in)"
    );

    let refused = fixture.refused(&["set", "tasks.merge", "checks", "--task", "T1"]);
    assert!(
        refused.contains("a task's own settings cannot be written from here"),
        "{refused}"
    );
    let refused = fixture.refused(&["get", "tasks.merge", "--task", "NOPE"]);
    assert!(refused.contains("no task `NOPE`"), "{refused}");
}

#[test]
fn a_role_is_never_stored_unchecked() {
    let fixture = Fixture::new();
    // No data directory: the harness table cannot be read, so a role write must
    // not be accepted with no warning at all.
    let refused = fixture.run_without_data(&["set", "roles.worker.agent", "claude"]);
    assert_eq!(refused.status.code(), Some(2), "{refused:?}");
    let stderr = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(
        stderr.contains("cannot be stored"),
        "the refusal says the role was not stored: {stderr}"
    );
    assert!(stderr.contains("SHPRD_DATA_DIR"), "{stderr}");
    assert!(
        !fixture.settings().exists(),
        "nothing was written without a check"
    );

    // A workflow key needs no harness table, so it is still storable.
    let stored = fixture.run_without_data(&["set", "tasks.max_running", "2"]);
    assert_eq!(
        stored.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&stored.stderr)
    );
}

#[test]
fn a_read_says_when_the_roles_could_not_be_checked() {
    let fixture = Fixture::new();
    Fixture::write(
        &fixture.settings(),
        "[roles.worker]\nagent = \"claude\"\n\n[roles.reviewer]\nagent = \"claude\"\n",
    );
    let output = fixture.run_without_data(&["show"]);
    assert!(output.status.success(), "reading never refuses");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("the roles were not checked"),
        "an unchecked read must say so: {stderr}"
    );
    let document = serde_json::from_str::<Value>(&String::from_utf8_lossy(
        &fixture.run_without_data(&["show", "--json"]).stdout,
    ))
    .expect("JSON");
    assert!(
        document["not_checked"].as_str().unwrap_or_default() != "",
        "the JSON says why the roles were not checked: {document}"
    );
    assert_eq!(document["problems"], serde_json::json!([]));

    // A checkout with no pick at all has nothing to check, so nothing is said.
    let quiet = Fixture::new();
    let output = quiet.run_without_data(&["show"]);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!stderr.contains("not checked"), "{stderr}");
}

#[test]
fn a_broken_pair_someone_left_does_not_block_other_keys() {
    let fixture = Fixture::new();
    Fixture::write(
        &fixture.settings(),
        "[roles.worker]\nagent = \"claude\"\n\n[roles.reviewer]\nagent = \"claude\"\n",
    );
    // The pair is already refused, so a write that introduces no new refusal
    // goes through: a broken file is not a lock on every other key.
    let printed = fixture.ok(&["set", "tasks.review_rounds", "3"]);
    assert!(
        printed.contains("ok tasks.review_rounds stored"),
        "{printed}"
    );
    // A write that introduces a refusal is still refused.
    let refused = fixture.refused(&["set", "roles.docs.agent", "gemini"]);
    assert!(refused.contains("no harness defines"), "{refused}");
    // And the pair itself is still reported on read.
    let output = fixture.run(&["show"]);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains("both anthropic"), "{stderr}");
}

#[test]
fn a_custom_role_id_may_be_any_id_and_case() {
    let fixture = Fixture::new();
    let printed = fixture.ok(&["set", "roles.Docs.kind", "worker"]);
    assert!(printed.contains("roles.Docs.kind"), "{printed}");
    let printed = fixture.ok(&["set", "roles.QA_2.agent", "codex"]);
    assert!(printed.contains("roles.QA_2.agent"), "{printed}");
    let document = json(&fixture.ok(&["show", "--json"]));
    assert_eq!(row(&document, "roles.Docs.kind")["value"], "worker");
    assert_eq!(row(&document, "roles.QA_2.agent")["value"], "codex");
    let refused = fixture.refused(&["set", "roles.my role.kind", "worker"]);
    assert!(refused.contains("no dots, spaces"), "{refused}");
}
