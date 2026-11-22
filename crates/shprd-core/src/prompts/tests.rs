//! The prompts' tests: first use, the upgrade of an untouched old default, an
//! edit that is never lost, reset, and the file behind a `prompt:` reference.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use tempfile::TempDir;

use super::*;

/// A temp prompts directory.
fn dir() -> TempDir {
    TempDir::new().expect("a temp prompts dir")
}

/// The shipped worker prompt.
fn worker() -> &'static ShippedPrompt {
    shipped_prompt("worker").expect("the worker prompt is shipped")
}

/// The file the worker prompt is installed at, in `dir`.
fn worker_path(dir: &Path) -> PathBuf {
    path_of(dir, "worker")
}

#[test]
fn first_use_writes_the_shipped_words() {
    let temp = dir();
    let installed = ensure(temp.path(), "worker").expect("the first use installs the prompt");
    assert_eq!(installed.change, Change::Created, "{installed:?}");
    assert_eq!(installed.state, State::Default, "{installed:?}");
    assert_eq!(installed.text, worker().text);
    let on_disk = std::fs::read_to_string(worker_path(temp.path())).expect("the file is there");
    assert_eq!(on_disk, worker().text, "the file holds the shipped words");
}

#[test]
fn a_second_use_leaves_the_file_alone() {
    let temp = dir();
    ensure(temp.path(), "worker").expect("first use");
    let again = ensure(temp.path(), "worker").expect("second use");
    assert_eq!(again.change, Change::Kept, "{again:?}");
    assert_eq!(again.state, State::Default, "{again:?}");
}

#[test]
fn an_untouched_old_default_is_upgraded() {
    let temp = dir();
    // An install from a build whose worker words were the first draft: the file
    // is byte-identical to a retired default and nobody edited it.
    std::fs::write(worker_path(temp.path()), WORKER_FIRST).expect("an old install");
    let installed = ensure(temp.path(), "worker").expect("the upgrade runs");
    assert_eq!(installed.change, Change::Upgraded, "{installed:?}");
    assert_eq!(installed.text, worker().text, "today's words won");
    let on_disk = std::fs::read_to_string(worker_path(temp.path())).expect("the file is there");
    assert_eq!(on_disk, worker().text, "the upgrade reached the disk");
}

#[test]
fn an_edited_file_is_never_overwritten() {
    let temp = dir();
    std::fs::write(worker_path(temp.path()), "my own words\n").expect("an edit");
    for _ in 0..2 {
        let installed = ensure(temp.path(), "worker").expect("the file is read");
        assert_eq!(installed.change, Change::Kept, "{installed:?}");
        assert_eq!(installed.state, State::Edited, "{installed:?}");
        assert_eq!(installed.text, "my own words\n");
    }
    let on_disk = std::fs::read_to_string(worker_path(temp.path())).expect("the file is there");
    assert_eq!(on_disk, "my own words\n", "the edit survived");
}

#[test]
fn reset_puts_the_shipped_words_back() {
    let temp = dir();
    std::fs::write(worker_path(temp.path()), "my own words\n").expect("an edit");
    let installed = reset(temp.path(), "worker").expect("reset");
    assert_eq!(installed.change, Change::Reset, "{installed:?}");
    assert_eq!(installed.state, State::Default, "{installed:?}");
    assert_eq!(installed.text, worker().text);
    assert_eq!(
        std::fs::read_to_string(worker_path(temp.path())).expect("the file is there"),
        worker().text
    );
    let again = reset(temp.path(), "worker").expect("a second reset");
    assert_eq!(again.change, Change::Unchanged, "{again:?}");
}

#[test]
fn a_write_saves_the_persons_words() {
    let temp = dir();
    let written = write(temp.path(), "worker", "mine\n").expect("a write");
    assert_eq!(written.change, Change::Written, "{written:?}");
    assert_eq!(written.state, State::Edited, "{written:?}");
    let again = write(temp.path(), "worker", "mine\n").expect("the same write");
    assert_eq!(again.change, Change::Unchanged, "{again:?}");
    let shipped_words = write(temp.path(), "worker", worker().text).expect("the shipped words");
    assert_eq!(shipped_words.state, State::Default, "{shipped_words:?}");
}

#[test]
fn a_custom_prompt_is_read_and_a_missing_one_is_refused() {
    let temp = dir();
    let missing = ensure(temp.path(), "docs").expect_err("no words to invent");
    let text = missing.to_string();
    assert!(text.contains("no prompt `docs`"), "{text}");
    assert!(
        text.contains("docs.md"),
        "the refusal names the file: {text}"
    );
    assert!(text.contains("worker"), "and the ones that exist: {text}");

    write(temp.path(), "docs", "write the docs\n").expect("the person's prompt");
    let installed = ensure(temp.path(), "docs").expect("the file is read");
    assert_eq!(installed.text, "write the docs\n");
    assert!(!installed.shipped, "a custom prompt is not shipped");
    assert_eq!(installed.state, State::Edited, "{installed:?}");
}

/// The prompts a listing holds, dropping their prose.
fn listed_ids(dir: &Path) -> Vec<String> {
    list(dir)
        .expect("list")
        .into_iter()
        .map(|listed| match listed {
            Listed::Prompt(installed) => installed.id,
            Listed::Unreadable { id, .. } => id,
        })
        .collect()
}

#[test]
fn list_writes_every_shipped_prompt_and_finds_the_persons_own() {
    let temp = dir();
    write(temp.path(), "docs", "write the docs\n").expect("the person's prompt");
    let ids = listed_ids(temp.path());
    assert_eq!(ids, ["assistant", "worker", "reviewer", "backup", "docs"]);
    for id in &ids {
        assert!(
            temp.path().join(format!("{id}.md")).exists(),
            "{id} was written on first use"
        );
    }
}

#[test]
fn an_unreadable_prompt_is_listed_with_its_reason() {
    let temp = dir();
    std::fs::write(temp.path().join("broken.md"), [0xff, 0xfe, 0x00]).expect("a broken file");
    let listed = list(temp.path()).expect("list");
    let broken = listed
        .iter()
        .find_map(|entry| match entry {
            Listed::Unreadable { id, path, reason } => Some((id, path, reason)),
            _ => None,
        })
        .expect("a file that cannot be read is still listed");
    assert_eq!(broken.0, "broken");
    assert_eq!(broken.1, &temp.path().join("broken.md"));
    assert!(broken.2.contains("not UTF-8 text"), "{}", broken.2);
    assert!(
        listed.iter().any(|entry| matches!(
            entry,
            Listed::Prompt(installed) if installed.id == "worker"
        )),
        "the readable prompts are listed as usual"
    );
}

#[test]
fn the_coordinator_is_the_assistant_prompt() {
    let coordinator = shipped_prompt("coordinator").expect("the coordinator is shipped");
    assert_eq!(coordinator.id, "assistant");
    assert_eq!(coordinator.role, "coordinator");
    assert_eq!(canonical("coordinator"), "assistant");
    assert_eq!(
        path_of(Path::new("/p"), "coordinator").file_name(),
        Some("assistant.md".as_ref())
    );
}

#[test]
fn an_id_that_could_leave_the_prompts_directory_is_refused() {
    let temp = dir();
    for bad in ["../worker", "a/b", "worker.md", ""] {
        let error = ensure(temp.path(), bad).expect_err("a bad id is refused");
        assert!(matches!(error, PromptError::BadId { .. }), "{bad}: {error}");
    }
}

#[test]
fn a_reference_reads_an_id_or_a_path() {
    let temp = dir();
    let home = TempDir::new().expect("a temp home");
    let by_id = PromptRef::parse("prompt:worker").expect("an id");
    assert_eq!(
        by_id
            .read(temp.path(), home.path())
            .expect("the shipped prompt"),
        strip_leading_comment(worker().text)
    );

    let file = temp.path().join("notes.md");
    std::fs::write(&file, "notes\n").expect("a file");
    let absolute = PromptRef::parse(file.to_str().expect("a utf-8 path")).expect("a path");
    assert_eq!(
        absolute
            .read(temp.path(), home.path())
            .expect("the file is read"),
        "notes\n"
    );

    std::fs::write(home.path().join("mine.md"), "mine\n").expect("a file in home");
    let tilde = PromptRef::parse("~/mine.md").expect("a path");
    assert_eq!(
        tilde
            .read(temp.path(), home.path())
            .expect("the file is read"),
        "mine\n",
        "a ~ in a role's prompt is the home directory"
    );

    assert!(matches!(
        PromptRef::parse("prompt:"),
        Err(PromptError::BadRef { .. })
    ));
    assert!(matches!(
        PromptRef::parse("  "),
        Err(PromptError::BadRef { .. })
    ));
}

#[test]
fn a_read_failure_names_the_file_once_and_keeps_its_cause() {
    let temp = dir();
    let home = TempDir::new().expect("a temp home");
    let missing = temp.path().join("gone.md");
    let reference = PromptRef::parse(missing.to_str().expect("a utf-8 path")).expect("a path");
    let error = reference
        .read(temp.path(), home.path())
        .expect_err("the file is not there");
    assert_eq!(
        error.to_string(),
        format!("cannot read {}", missing.display())
    );
    assert!(
        std::error::Error::source(&error).is_some(),
        "the OS error stays in the chain, so a caller can still see why"
    );
}

#[test]
fn the_worker_first_digest_is_the_listed_one() {
    assert_eq!(digest(WORKER_FIRST.as_bytes()), WORKER_FIRST_DIGEST);
    assert!(
        worker().old.contains(&WORKER_FIRST_DIGEST),
        "the retired default is in the worker's list"
    );
    assert!(WORKER_FIRST.as_bytes() != worker().text.as_bytes());
}

#[test]
fn the_worker_m9_digest_is_the_listed_one() {
    assert_eq!(digest(WORKER_M9.as_bytes()), WORKER_M9_DIGEST);
    assert!(
        worker().old.contains(&WORKER_M9_DIGEST),
        "the retired default is in the worker's list"
    );
    assert!(WORKER_M9.as_bytes() != worker().text.as_bytes());
}

#[test]
fn the_worker_m9_asks_digest_is_the_listed_one() {
    assert_eq!(digest(WORKER_M9_ASKS.as_bytes()), WORKER_M9_ASKS_DIGEST);
    assert!(
        worker().old.contains(&WORKER_M9_ASKS_DIGEST),
        "the retired default is in the worker's list"
    );
    assert!(WORKER_M9_ASKS.as_bytes() != worker().text.as_bytes());
}

#[test]
fn the_worker_m9_proof_digest_is_the_listed_one() {
    assert_eq!(digest(WORKER_M9_PROOF.as_bytes()), WORKER_M9_PROOF_DIGEST);
    assert!(
        worker().old.contains(&WORKER_M9_PROOF_DIGEST),
        "the retired default is in the worker's list"
    );
    assert!(WORKER_M9_PROOF.as_bytes() != worker().text.as_bytes());
}

/// A build that installed the wording which put the proof rule after the
/// hand-in rule upgrades it: a hand-in is spent once, so the rule has to be
/// read before the report.
#[test]
fn an_untouched_proof_default_is_upgraded() {
    let temp = dir();
    std::fs::write(worker_path(temp.path()), WORKER_M9_PROOF).expect("an old install");
    let installed = ensure(temp.path(), "worker").expect("the upgrade runs");
    assert_eq!(installed.change, Change::Upgraded, "{installed:?}");
    assert_eq!(installed.text, worker().text, "today's words won");
}

#[test]
fn the_worker_m10_proof_digest_is_the_listed_one() {
    assert_eq!(digest(WORKER_M10_PROOF.as_bytes()), WORKER_M10_PROOF_DIGEST);
    assert!(
        worker().old.contains(&WORKER_M10_PROOF_DIGEST),
        "the retired default is in the worker's list"
    );
    assert!(WORKER_M10_PROOF.as_bytes() != worker().text.as_bytes());
}

/// The wording which named `--proof` but not where the artifact lives upgrades
/// too: a proof outside the worktree is one the reviewer cannot open.
#[test]
fn an_untouched_m10_proof_default_is_upgraded() {
    let temp = dir();
    std::fs::write(worker_path(temp.path()), WORKER_M10_PROOF).expect("an old install");
    let installed = ensure(temp.path(), "worker").expect("the upgrade runs");
    assert_eq!(installed.change, Change::Upgraded, "{installed:?}");
    assert!(
        installed.text.contains("inside the task's worktree"),
        "the artifact has a place to live in today's words: {}",
        installed.text
    );
}

#[test]
fn the_reviewers_m9_asks_digest_is_the_listed_one() {
    let reviewer = shipped_prompt("reviewer").expect("the reviewer prompt is shipped");
    assert_eq!(digest(REVIEWER_M9_ASKS.as_bytes()), REVIEWER_M9_ASKS_DIGEST);
    assert!(
        reviewer.old.contains(&REVIEWER_M9_ASKS_DIGEST),
        "the retired default is in the reviewer's list"
    );
    assert!(REVIEWER_M9_ASKS.as_bytes() != reviewer.text.as_bytes());
}

/// The reviewer's words now point at the asks the packet carries, so an
/// install of the older wording is upgraded rather than left without them.
#[test]
fn an_untouched_reviewer_default_is_upgraded() {
    let temp = dir();
    let path = path_of(temp.path(), "reviewer");
    std::fs::write(&path, REVIEWER_M9_ASKS).expect("an old install");
    let installed = ensure(temp.path(), "reviewer").expect("the upgrade runs");
    assert_eq!(installed.change, Change::Upgraded, "{installed:?}");
    assert_eq!(
        installed.text,
        shipped_prompt("reviewer").expect("shipped").text,
        "today's words won"
    );
}

/// A build that installed the wording which only knew the CLI's question tool
/// upgrades it: the ask route is the program's, and the old words say otherwise.
#[test]
fn an_untouched_asks_default_is_upgraded() {
    let temp = dir();
    std::fs::write(worker_path(temp.path()), WORKER_M9_ASKS).expect("an old install");
    let installed = ensure(temp.path(), "worker").expect("the upgrade runs");
    assert_eq!(installed.change, Change::Upgraded, "{installed:?}");
    assert_eq!(installed.text, worker().text, "today's words won");
}

#[test]
fn the_backup_m9_asks_digest_is_the_listed_one() {
    let backup = shipped_prompt("backup").expect("the backup prompt is shipped");
    assert_eq!(digest(BACKUP_M9_ASKS.as_bytes()), BACKUP_M9_ASKS_DIGEST);
    assert!(
        backup.old.contains(&BACKUP_M9_ASKS_DIGEST),
        "the retired default is in the backup's list"
    );
    assert!(BACKUP_M9_ASKS.as_bytes() != backup.text.as_bytes());
}

#[test]
fn the_coordinators_m9_asks_digest_is_the_listed_one() {
    let assistant = shipped_prompt("assistant").expect("the coordinator prompt is shipped");
    assert_eq!(
        digest(ASSISTANT_M9_ASKS.as_bytes()),
        ASSISTANT_M9_ASKS_DIGEST
    );
    assert!(
        assistant.old.contains(&ASSISTANT_M9_ASKS_DIGEST),
        "the retired default is in the coordinator's list"
    );
    assert!(ASSISTANT_M9_ASKS.as_bytes() != assistant.text.as_bytes());
}

#[test]
fn the_coordinators_asks_raise_digest_is_the_listed_one() {
    let assistant = shipped_prompt("assistant").expect("the coordinator prompt is shipped");
    assert_eq!(
        digest(ASSISTANT_ASKS_RAISE.as_bytes()),
        ASSISTANT_ASKS_RAISE_DIGEST
    );
    assert!(
        assistant.old.contains(&ASSISTANT_ASKS_RAISE_DIGEST),
        "the retired default is in the coordinator's list"
    );
    assert!(ASSISTANT_ASKS_RAISE.as_bytes() != assistant.text.as_bytes());
}

/// A build that installed the wording which sent a hookless coordinator to
/// `shprd ask raise` upgrades it: that verb needs a task, and a coordinator has
/// none, so the route it promised could not work.
#[test]
fn an_untouched_asks_raise_default_is_upgraded() {
    let temp = dir();
    let path = path_of(temp.path(), "assistant");
    std::fs::write(&path, ASSISTANT_ASKS_RAISE).expect("an old install");
    let installed = ensure(temp.path(), "assistant").expect("the upgrade runs");
    assert_eq!(installed.change, Change::Upgraded, "{installed:?}");
    assert_eq!(
        installed.text,
        shipped_prompt("assistant").expect("shipped").text,
        "today's words won"
    );
}

/// The coordinator has no task (`$SHPRD_TASK` is set only for a task), so its
/// run-wide question must name a verb that works without one: `shprd agent
/// escalate` takes no task and shows on the board as waiting for the person.
#[test]
fn the_coordinator_is_not_sent_to_the_task_only_raise_verb() {
    let assistant = shipped_prompt("assistant").expect("the coordinator prompt is shipped");
    let text = assistant.text;
    assert!(
        text.contains("shprd agent escalate"),
        "the coordinator needs a verb that takes no task: {text}"
    );
    assert!(
        !text.contains("with `shprd ask raise`"),
        "`ask raise` needs a task the coordinator has not got: {text}"
    );
    // The task-only verb may still be named, but only as something a task's
    // question uses, with the reason spelled out.
    assert!(
        text.contains("`shprd ask raise` needs one"),
        "the wording must say why a card is not the coordinator's route: {text}"
    );
}

/// The worker's words point at the brief for the mechanism, because only the
/// brief knows the program's harness; naming one CLI's tool would be wrong for
/// every other program.
#[test]
fn the_worker_prompt_points_at_the_brief_for_how_to_ask() {
    let text = worker().text;
    assert!(text.contains("`Asking the person` section"), "{text}");
    assert!(
        !text.contains("ask with your CLI's own question tool"),
        "the mechanism is the brief's to name: {text}"
    );
}

#[test]
fn the_worker_prompt_hands_in_and_never_starts_its_own_review() {
    // Decided in prose: the worker hands in; the engine reviews. A brief that
    // told the worker to run the review itself would be a wrong instruction.
    let text = worker().text;
    assert!(
        !text.contains("shprd agent review"),
        "the worker is not told to review: {text}"
    );
    assert!(
        text.contains("Reviewing is the engine's job"),
        "the review belongs to the engine: {text}"
    );
    assert!(
        text.contains("hand in"),
        "the worker's job is to hand in: {text}"
    );
}

#[test]
fn every_shipped_prompt_credits_shepherdr() {
    for prompt in shipped() {
        assert!(
            prompt.text.starts_with("<!--"),
            "{} starts with its credit",
            prompt.id
        );
        assert!(
            prompt.text.contains("eliasstravik/shepherdr") && prompt.text.contains("MIT"),
            "{} names the source and its licence",
            prompt.id
        );
        assert!(
            !prompt.text.trim_end().is_empty(),
            "{} has words",
            prompt.id
        );
    }
}

#[test]
fn the_config_root_prefers_the_xdg_setting_over_the_home_one() {
    // The production rule, with the environment's value handed in: a set
    // `$XDG_CONFIG_HOME` wins and an unset or empty one falls back to
    // `<home>/.config`.
    assert_eq!(
        config_home(Path::new("/home/me"), Some(OsStr::new("/xdg/config"))),
        Path::new("/xdg/config")
    );
    assert_eq!(
        config_home(Path::new("/home/me"), None),
        Path::new("/home/me/.config")
    );
    assert_eq!(
        config_home(Path::new("/home/me"), Some(OsStr::new(""))),
        Path::new("/home/me/.config"),
        "an empty setting is unset"
    );
}

#[test]
fn the_prompts_directory_sits_under_the_config_root() {
    assert_eq!(
        prompts_dir(&config_home(Path::new("/home/me"), None)),
        Path::new("/home/me/.config/shprd/prompts")
    );
    assert_eq!(
        prompts_dir(&config_home(
            Path::new("/home/me"),
            Some(OsStr::new("/xdg"))
        )),
        Path::new("/xdg/shprd/prompts")
    );
}

/// SHPR-66: every role's words open by saying it is shprd's, so another
/// product's session note in the same CLI cannot make it answer as that one.
#[test]
fn every_role_opens_with_who_it_is() {
    for (id, first) in [
        ("assistant", "You are shprd's coordinator"),
        ("worker", "You are a shprd worker"),
        ("reviewer", "You are a shprd reviewer"),
    ] {
        let text = shipped_prompt(id).expect("shipped").text;
        let body = text.split("-->").nth(1).expect("a licence header");
        assert!(body.trim_start().starts_with(first), "{id}: {body:.80}");
        assert!(text.contains("Shepherdr"), "{id} names what it is not");
    }
}

#[test]
fn the_pre_identity_defaults_are_retired_and_upgrade() {
    for (id, old, old_digest) in [
        (
            "assistant",
            ASSISTANT_PRE_IDENTITY,
            ASSISTANT_PRE_IDENTITY_DIGEST,
        ),
        ("worker", WORKER_PRE_IDENTITY, WORKER_PRE_IDENTITY_DIGEST),
        (
            "reviewer",
            REVIEWER_PRE_IDENTITY,
            REVIEWER_PRE_IDENTITY_DIGEST,
        ),
    ] {
        let shipped = shipped_prompt(id).expect("shipped");
        assert_eq!(digest(old.as_bytes()), old_digest, "{id}");
        assert!(shipped.old.contains(&old_digest), "{id}");
        let temp = dir();
        std::fs::create_dir_all(temp.path()).expect("dir");
        std::fs::write(temp.path().join(format!("{id}.md")), old).expect("an old install");
        let installed = ensure(temp.path(), id).expect("the upgrade runs");
        assert_eq!(installed.change, Change::Upgraded, "{id}: {installed:?}");
    }
}

/// SHPR-71: the identity-era coordinator wording is retired and upgrades.
#[test]
fn the_identity_era_coordinator_words_are_retired_and_upgrade() {
    let shipped = shipped_prompt("assistant").expect("shipped");
    assert_eq!(
        digest(ASSISTANT_M11_IDENTITY.as_bytes()),
        ASSISTANT_M11_IDENTITY_DIGEST
    );
    assert!(shipped.old.contains(&ASSISTANT_M11_IDENTITY_DIGEST));
    assert!(ASSISTANT_M11_IDENTITY != shipped.text);
    let temp = dir();
    std::fs::create_dir_all(temp.path()).expect("dir");
    std::fs::write(temp.path().join("assistant.md"), ASSISTANT_M11_IDENTITY)
        .expect("an old install");
    let installed = ensure(temp.path(), "assistant").expect("the upgrade runs");
    assert_eq!(installed.change, Change::Upgraded, "{installed:?}");
}

/// SHPR-71: whatever the ask's wording, the coordinator makes tasks and never
/// edits; several items are several tasks; a dependency is `--dep`; a question
/// the person wants asked is the task's first acceptance line.
#[test]
fn the_coordinator_turns_every_ask_into_tasks_and_never_does_the_work() {
    let text = shipped_prompt("assistant").expect("shipped").text;
    for needed in [
        "Every ask that changes the repository becomes tasks, however it is worded",
        "\"Add a hello.py that prints hi\"",
        "\"do it yourself, it is one line\"",
        "I'll make that a task",
        "Never create, edit, move or delete a file",
        "never run `git add`, `git commit`",
        "one task per item",
        "`--dep <ID>`",
        "asks the person that question before it changes anything",
        "Questions about the repository are yours to answer by reading it",
        "do not look for a way around that with the shell",
    ] {
        assert!(text.contains(needed), "the brief must say {needed:?}");
    }
}

/// SHPR-74: the tasks-only coordinator wording is retired and upgrades.
#[test]
fn the_tasks_only_coordinator_words_are_retired_and_upgrade() {
    let shipped = shipped_prompt("assistant").expect("shipped");
    assert_eq!(
        digest(ASSISTANT_M11_TASKS_ONLY.as_bytes()),
        ASSISTANT_M11_TASKS_ONLY_DIGEST
    );
    assert!(shipped.old.contains(&ASSISTANT_M11_TASKS_ONLY_DIGEST));
    assert!(ASSISTANT_M11_TASKS_ONLY != shipped.text);
    let temp = dir();
    std::fs::create_dir_all(temp.path()).expect("dir");
    std::fs::write(temp.path().join("assistant.md"), ASSISTANT_M11_TASKS_ONLY)
        .expect("an old install");
    let installed = ensure(temp.path(), "assistant").expect("the upgrade runs");
    assert_eq!(installed.change, Change::Upgraded, "{installed:?}");
}

/// SHPR-74: the coordinator points the person at the Tasks pane to accept, never
/// at a command or the forge.
#[test]
fn the_coordinator_says_accept_in_the_tasks_pane() {
    let text = shipped_prompt("assistant").expect("shipped").text;
    assert!(text.contains("Accept it in the Tasks pane"));
    assert!(text.contains("Never tell them to run `shprd approve`"));
}

/// SHPR-79: the coordinator never uses Shepherdr's tools, and the wording that
/// did not forbid them is retired and upgrades.
#[test]
fn the_coordinator_never_uses_shepherdrs_tools_and_the_old_words_upgrade() {
    let shipped = shipped_prompt("assistant").expect("shipped");
    for needed in [
        "Never run `herdr-task`",
        "Every task is made with `shprd agent task add`",
    ] {
        assert!(
            shipped.text.contains(needed),
            "the brief must say {needed:?}"
        );
    }
    assert_eq!(
        digest(ASSISTANT_M11_ACCEPT.as_bytes()),
        ASSISTANT_M11_ACCEPT_DIGEST
    );
    assert!(shipped.old.contains(&ASSISTANT_M11_ACCEPT_DIGEST));
    let temp = dir();
    std::fs::create_dir_all(temp.path()).expect("dir");
    std::fs::write(temp.path().join("assistant.md"), ASSISTANT_M11_ACCEPT).expect("an old install");
    let installed = ensure(temp.path(), "assistant").expect("the upgrade runs");
    assert_eq!(installed.change, Change::Upgraded, "{installed:?}");
}

#[test]
fn the_pre_shpr83_assistant_digest_is_the_listed_one() {
    let assistant = shipped_prompt("assistant").expect("shipped");
    assert_eq!(
        digest(ASSISTANT_PRE_SHPR83.as_bytes()),
        ASSISTANT_PRE_SHPR83_DIGEST
    );
    assert!(assistant.old.contains(&ASSISTANT_PRE_SHPR83_DIGEST));
}

/// The text shipped before SHPR-83 read `edited` forever; it upgrades now.
#[test]
fn an_untouched_pre_shpr83_assistant_is_upgraded() {
    let temp = dir();
    std::fs::write(path_of(temp.path(), "assistant"), ASSISTANT_PRE_SHPR83).expect("old install");
    let installed = ensure(temp.path(), "assistant").expect("the upgrade runs");
    assert_eq!(installed.change, Change::Upgraded, "{installed:?}");
    assert_eq!(installed.state, State::Default);
    assert_eq!(
        installed.text,
        shipped_prompt("assistant").expect("shipped").text
    );
}

/// The digest of each shipped template the last time someone edited it. When a
/// template changes, this stays the old digest, which must then be in the
/// prompt's retired list: the next edit cannot strand an install. After adding
/// the retired digest, replace the pin with the new current digest.
const PINNED: [(&str, &str); 4] = [
    (
        "assistant",
        "391d4cc326bf5fd542a2486474a1496d9da08bb2f77294fd4ca7d0a0740a0e5f",
    ),
    (
        "worker",
        "d9a7bb2a42b8245b45d85b172eb659138329186079b7d4bd2303a202e48df9fa",
    ),
    (
        "reviewer",
        "c187a5267112fc4148487499f64ef7ac444e10bce3a0a2a901bd38daf2ef0488",
    ),
    (
        "backup",
        "4cbcd2fd8c152a6c734421c5831cafb33c5a9549bcbfa5d255302f011761c485",
    ),
];

#[test]
fn a_shipped_templates_previous_digest_is_retired() {
    for (id, pinned) in PINNED {
        let prompt = shipped_prompt(id).expect("shipped");
        let current = digest(prompt.text.as_bytes());
        assert!(
            pinned == current || prompt.old.contains(&pinned),
            "`{id}` changed (now {current}, pinned {pinned}): add {pinned} to its retired list, keep the old text under data/templates/history/, then pin {current}"
        );
    }
}

#[test]
fn the_loader_drops_the_credit_comment_and_the_file_keeps_it() {
    let temp = dir();
    let home = TempDir::new().expect("home");
    for prompt in shipped() {
        let words = PromptRef::Prompt(prompt.id.to_owned())
            .read(temp.path(), home.path())
            .expect("reads");
        assert!(!words.contains("<!--") && !words.contains("-->"), "{words}");
        assert!(!words.contains("eliasstravik"), "{}", prompt.id);
        assert!(!words.trim().is_empty());
        let on_disk = std::fs::read_to_string(path_of(temp.path(), prompt.id)).expect("file");
        assert!(on_disk.starts_with("<!--"), "the file keeps its credit");
    }
    assert_eq!(strip_leading_comment("<!-- a\nb -->\n\nhello"), "hello");
    assert_eq!(
        strip_leading_comment("plain <!-- x -->"),
        "plain <!-- x -->"
    );
    assert_eq!(strip_leading_comment("<!-- open"), "<!-- open");
}
