//! The settings model's tests: layers, sources, refusals and the file on disk.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use tempfile::TempDir;

use super::*;

/// A temp home directory.
fn home() -> TempDir {
    TempDir::new().expect("a temp home")
}

/// The layer a `[tasks]`/`[review]` snippet makes, as the person's defaults.
fn you(text: &str) -> File {
    File::parse(text).expect("the test settings parse")
}

/// The value one key resolves to, as a settings line prints it.
fn shown(resolved: &Resolved, key: &str) -> String {
    let key = Key::parse(key).expect("the key parses");
    resolved
        .get(&key)
        .map(|sourced| sourced.value.display())
        .unwrap_or_else(|| "(unset)".to_owned())
}

/// The layer one key resolves from.
fn source_of(resolved: &Resolved, key: &str) -> Option<Source> {
    let key = Key::parse(key).expect("the key parses");
    resolved.source_of(&key)
}

#[test]
fn an_empty_file_resolves_to_the_built_in_values() {
    let resolved = resolve(&File::default(), &Request::default());
    assert_eq!(shown(&resolved, "tasks.merge"), "\"ask\"");
    assert_eq!(shown(&resolved, "tasks.max_running"), "4");
    assert_eq!(shown(&resolved, "tasks.review_rounds"), "2");
    assert_eq!(shown(&resolved, "tasks.must_fix"), "\"major\"");
    assert_eq!(shown(&resolved, "tasks.archive_after"), "14");
    assert_eq!(shown(&resolved, "tasks.archive_namespace"), "\"shprd/\"");
    assert_eq!(shown(&resolved, "tasks.tick_secs"), "15");
    assert_eq!(shown(&resolved, "tasks.news_settle_secs"), "5");
    assert_eq!(shown(&resolved, "tasks.proof"), "true");
    assert_eq!(shown(&resolved, "tasks.worktrees"), "true");
    assert_eq!(shown(&resolved, "tasks.base"), "\"\"");
    assert_eq!(shown(&resolved, "tasks.branch"), "\"task/{n}-{slug}\"");
    assert_eq!(shown(&resolved, "review.cross_vendor"), "true");
    assert_eq!(shown(&resolved, "host"), "\"auto\"");
    assert_eq!(
        source_of(&resolved, "tasks.merge"),
        Some(Source::BuiltIn),
        "a value nobody set is built in"
    );
    for key in ["tasks.merge", "tasks.max_running", "review.cross_vendor"] {
        assert_eq!(source_of(&resolved, key), Some(Source::BuiltIn), "{key}");
    }
}

#[test]
fn the_built_in_workflow_values_are_the_old_config_defaults() {
    // Folding the old keys in must not move a value: the settings model's
    // defaults are the config types' own defaults.
    let config = Config::default();
    let tasks = resolve(&File::default(), &Request::default()).tasks();
    assert_eq!(tasks.merge, config.land.merge);
    assert_eq!(tasks.review_rounds, config.review.rounds);
    assert_eq!(tasks.must_fix, config.review.must_fix);
    assert_eq!(tasks.max_running, config.tick.max_running);
    assert_eq!(tasks.archive_after, config.janitor.archive_days);
    assert_eq!(tasks.archive_namespace, config.janitor.namespace);
    assert_eq!(tasks.tick_secs, config.tick.interval_secs);
    assert!(tasks.proof && tasks.worktrees);
}

#[test]
fn the_four_built_in_roles_are_the_first_rows() {
    let resolved = resolve(&File::default(), &Request::default());
    assert_eq!(
        resolved.order(),
        ["coordinator", "worker", "reviewer", "backup"],
        "the three built-ins and the backup, in their own order"
    );
    let coordinator = resolved.role("coordinator").expect("the coordinator");
    assert_eq!(coordinator.label(), "Assistant");
    assert_eq!(coordinator.kind.value, RoleKind::Coordinator);
    assert_eq!(coordinator.prompt.value, "prompt:assistant");
    let reviewer = resolved.role("reviewer").expect("the reviewer");
    assert_eq!(reviewer.kind.value, RoleKind::Reviewer);
    assert!(reviewer.readonly.value, "the reviewer only reads");
    assert!(reviewer.may_review());
    assert!(!resolved.role("worker").expect("the worker").may_review());
    // A built-in role ships with its identity and no pick at all.
    assert!(
        coordinator.agent.is_none(),
        "no program is chosen by default"
    );
    assert!(coordinator.model.is_none());
    assert!(coordinator.fallback.value.is_empty());
}

#[test]
fn a_custom_role_is_any_id_and_defaults_to_a_helper() {
    let file = you(r#"
        version = 1

        [roles.docs]
        title = "Docs writer"
        about = "Writes and fixes documentation only."
        kind = "worker"
        prompt = "~/.config/shprd/prompts/docs.md"
        agent = "pi"
        model = "anthropic/claude-sonnet-5-5"
        effort = "medium"
        perm = "read-only"
        fallback = ["claude"]
        "#);
    let resolved = resolve(&file, &Request::default());
    assert_eq!(
        resolved.order(),
        ["coordinator", "worker", "reviewer", "backup", "docs"],
        "a custom role follows the built-ins unless the order says otherwise"
    );
    let docs = resolved.role("docs").expect("the docs role");
    assert_eq!(docs.label(), "Docs writer");
    assert_eq!(docs.kind.value, RoleKind::Worker);
    assert_eq!(docs.agent.as_ref().map(|a| a.value.as_str()), Some("pi"));
    assert_eq!(
        docs.fallback.value,
        vec!["claude".to_owned()],
        "the fallback list is the role's"
    );
    assert_eq!(docs.prompt.source, Source::You);

    // An id nobody describes still exists as a role, and safely: a helper can
    // neither dispatch nor review.
    let bare = you("[roles.spare]\nagent = \"codex\"\n");
    let resolved = resolve(&bare, &Request::default());
    let spare = resolved.role("spare").expect("the spare role");
    assert_eq!(spare.kind.value, RoleKind::Helper);
    assert_eq!(spare.label(), "spare");
    assert_eq!(spare.prompt.value, "prompt:spare");
    assert!(!spare.may_review());
}

#[test]
fn roles_order_leads_and_every_other_role_follows() {
    let file = you(r#"
        [roles]
        order = ["docs", "coordinator", "worker"]

        [roles.docs]
        kind = "worker"

        [roles.spare]
        kind = "helper"
        "#);
    let resolved = resolve(&file, &Request::default());
    assert_eq!(
        resolved.order(),
        [
            "docs",
            "coordinator",
            "worker",
            "reviewer",
            "backup",
            "spare"
        ],
        "the order leads, the built-ins it left out follow, then the rest"
    );
}

#[test]
fn a_task_layer_beats_the_repo_entry_which_beats_you() {
    let file = File::parse(
        r#"
        [tasks]
        merge = "checks"

        [repos.mine.tasks]
        merge = "auto"
        "#,
    )
    .expect("the file parses");
    let mut task = Layer::new();
    task.set(Key::Task(TaskKey::Merge), Value::Text("ask".to_owned()));

    let all = resolve(
        &file,
        &Request {
            repo: Some("mine"),
            repo_file: None,
            task: Some(&task),
        },
    );
    assert_eq!(shown(&all, "tasks.merge"), "\"ask\"");
    assert_eq!(source_of(&all, "tasks.merge"), Some(Source::Task));

    let without_a_task = resolve(
        &file,
        &Request {
            repo: Some("mine"),
            repo_file: None,
            task: None,
        },
    );
    assert_eq!(shown(&without_a_task, "tasks.merge"), "\"auto\"");
    assert_eq!(
        source_of(&without_a_task, "tasks.merge"),
        Some(Source::RepoEntry)
    );

    let other_repo = resolve(
        &file,
        &Request {
            repo: Some("another"),
            repo_file: None,
            task: None,
        },
    );
    assert_eq!(shown(&other_repo, "tasks.merge"), "\"checks\"");
    assert_eq!(source_of(&other_repo, "tasks.merge"), Some(Source::You));
}

#[test]
fn the_repos_committed_file_sits_under_you_and_over_the_built_ins() {
    let document = r#"
        version = 1

        [tasks]
        base = "trunk"
        "#;
    let repo_file = parse_repo_committed(document).expect("the repo file parses");
    let resolved = resolve(
        &File::default(),
        &Request {
            repo: None,
            repo_file: Some(&repo_file),
            task: None,
        },
    );
    assert_eq!(shown(&resolved, "tasks.base"), "\"trunk\"");
    assert_eq!(source_of(&resolved, "tasks.base"), Some(Source::RepoFile));

    // The person's own value wins over the repository's file.
    let file = you("[tasks]\nbase = \"mine\"\n");
    let resolved = resolve(
        &file,
        &Request {
            repo: None,
            repo_file: Some(&repo_file),
            task: None,
        },
    );
    assert_eq!(shown(&resolved, "tasks.base"), "\"mine\"");
    assert_eq!(source_of(&resolved, "tasks.base"), Some(Source::You));
}

#[test]
fn every_key_is_reported_even_the_ones_nobody_set() {
    let resolved = resolve(&File::default(), &Request::default());
    let rows = resolved.rows();
    let key = |name: &str| {
        rows.iter()
            .find(|row| row.key == name)
            .unwrap_or_else(|| panic!("`{name}` should be a row: {:?}", rows.len()))
    };
    assert_eq!(key("roles.worker.agent").value, None);
    assert_eq!(key("roles.worker.agent").source, None);
    assert!(key("tasks.merge").value.is_some());
    assert_eq!(
        key("roles.order").value,
        Some(Value::List(
            BUILTIN_ROLES.iter().map(|id| (*id).to_owned()).collect()
        ))
    );
    // The rows come in the order a settings page draws them.
    assert_eq!(rows[0].key, "roles.order");
    assert_eq!(rows[1].key, "roles.coordinator.title");
    assert!(rows.iter().any(|row| row.key == "tasks.tick_secs"));
    assert_eq!(
        rows.last().map(|row| row.key.as_str()),
        Some("web.cloudflared_host"),
        "the web keys are the last rows a settings page draws"
    );
    assert!(rows.iter().any(|row| row.key == "host"));
}

#[test]
fn a_value_equal_to_the_inherited_one_is_not_stored() {
    let home = home();
    let loaded = Loaded::read(home.path(), None).expect("an empty home loads");
    let key = Key::Task(TaskKey::Merge);
    let stored = loaded
        .write(
            home.path(),
            &Scope::You,
            &key,
            Some(Value::Text("checks".to_owned())),
            None,
        )
        .expect("the write succeeds");
    assert_eq!(stored.outcome, WriteOutcome::Stored);
    assert_eq!(stored.resolved.tasks().merge, MergePolicy::Checks);
    let text = std::fs::read_to_string(user_file(home.path())).expect("the file was written");
    assert!(text.contains("merge = \"checks\""), "{text}");

    // Setting it back to what the built-in layer says removes it instead.
    let again = Loaded::read(home.path(), None).expect("the file loads");
    let reset = again
        .write(
            home.path(),
            &Scope::You,
            &key,
            Some(Value::Text("ask".to_owned())),
            None,
        )
        .expect("the write succeeds");
    assert_eq!(reset.outcome, WriteOutcome::EqualToInherited);
    let text = std::fs::read_to_string(user_file(home.path())).expect("the file is still there");
    assert!(
        !text.contains("merge"),
        "the equal value is not stored: {text}"
    );
}

#[test]
fn unset_removes_the_key_and_leaves_the_file_readable() {
    let home = home();
    let loaded = Loaded::read(home.path(), None).expect("an empty home loads");
    let key = Key::Task(TaskKey::MaxRunning);
    loaded
        .write(home.path(), &Scope::You, &key, Some(Value::Number(2)), None)
        .expect("the write succeeds");

    let loaded = Loaded::read(home.path(), None).expect("the file loads");
    let removed = loaded
        .write(home.path(), &Scope::You, &key, None, None)
        .expect("the unset succeeds");
    assert_eq!(removed.outcome, WriteOutcome::Removed);
    assert_eq!(removed.resolved.tasks().max_running, 4);
    let text = std::fs::read_to_string(user_file(home.path())).expect("the file was written");
    assert!(!text.contains("max_running"), "{text}");
    assert!(text.contains("version = 1"), "the file is stamped: {text}");

    let again = Loaded::read(home.path(), None).expect("the file loads");
    let nothing = again
        .write(home.path(), &Scope::You, &key, None, None)
        .expect("the second unset succeeds");
    assert_eq!(nothing.outcome, WriteOutcome::WasUnset);
}

#[test]
fn a_file_that_changed_while_it_was_being_edited_is_refused() {
    let home = home();
    let path = user_file(home.path());
    std::fs::create_dir_all(path.parent().expect("the config dir")).expect("the dir");
    std::fs::write(&path, "[tasks]\nmerge = \"checks\"\n").expect("the file is written");
    let loaded = Loaded::read(home.path(), None).expect("the file loads");

    // Someone else — the service, an editor — saves the file first.
    let theirs = "[tasks]\nmerge = \"auto\"\n";
    std::fs::write(&path, theirs).expect("the file is written again");
    let refused = loaded.write(
        home.path(),
        &Scope::You,
        &Key::Task(TaskKey::MaxRunning),
        Some(Value::Number(2)),
        None,
    );
    match refused {
        Err(SettingsError::Changed { path: named }) => assert_eq!(named, path),
        other => panic!("expected a changed-file refusal, got {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file is there"),
        theirs,
        "the other writer's file is left exactly as it was"
    );
}

#[test]
fn an_unset_of_a_key_nobody_set_does_not_create_a_file() {
    let home = home();
    let loaded = Loaded::read(home.path(), None).expect("an empty home loads");
    let outcome = loaded
        .write(
            home.path(),
            &Scope::You,
            &Key::Task(TaskKey::Merge),
            None,
            None,
        )
        .expect("the unset succeeds");
    assert_eq!(outcome.outcome, WriteOutcome::WasUnset);
    assert!(!user_file(home.path()).exists());
}

#[test]
fn a_repo_entry_is_written_under_repos_and_read_back() {
    let home = home();
    let repo = home.path().join("shprd");
    std::fs::create_dir_all(&repo).expect("the repo dir");
    let loaded = Loaded::read(home.path(), Some(&repo)).expect("the empty home loads");
    assert_eq!(loaded.repo.as_deref(), Some("shprd"));
    loaded
        .write(
            home.path(),
            &Scope::Repo("shprd".to_owned()),
            &Key::Task(TaskKey::Merge),
            Some(Value::Text("auto".to_owned())),
            None,
        )
        .expect("the write succeeds");
    let text = std::fs::read_to_string(user_file(home.path())).expect("the file was written");
    assert!(text.contains("[repos.shprd.tasks]"), "{text}");
    assert!(text.contains("merge = \"auto\""), "{text}");

    let loaded = Loaded::read(home.path(), Some(&repo)).expect("the file loads");
    let resolved = loaded.resolve(None);
    assert_eq!(shown(&resolved, "tasks.merge"), "\"auto\"");
    assert_eq!(source_of(&resolved, "tasks.merge"), Some(Source::RepoEntry));
}

#[test]
fn a_write_keeps_the_comments_and_keys_it_does_not_own() {
    let home = home();
    let path = user_file(home.path());
    std::fs::create_dir_all(path.parent().expect("the config dir")).expect("the dir");
    std::fs::write(
        &path,
        "# written by hand\n[tasks]\nmerge = \"checks\"\n\n[review]\ncross_vendor = false\n",
    )
    .expect("the file is written");
    let loaded = Loaded::read(home.path(), None).expect("the file loads");
    loaded
        .write(
            home.path(),
            &Scope::You,
            &Key::Task(TaskKey::MaxRunning),
            Some(Value::Number(2)),
            None,
        )
        .expect("the write succeeds");
    let text = std::fs::read_to_string(&path).expect("the file is there");
    assert!(text.contains("# written by hand"), "{text}");
    assert!(text.contains("max_running = 2"), "{text}");
    // A key the verb does not own is left exactly as it was: a settings file is
    // the person's, and a verb only edits what it was asked to edit.
    assert!(text.contains("[review]\ncross_vendor = false"), "{text}");
}

#[test]
fn a_settings_file_that_does_not_parse_is_refused_by_name() {
    let home = home();
    let path = user_file(home.path());
    std::fs::create_dir_all(path.parent().expect("the config dir")).expect("the dir");
    std::fs::write(&path, "[tasks]\nmerge = \"sometimes\"\n").expect("the file is written");
    match Loaded::read(home.path(), None) {
        Err(SettingsError::InFile {
            path: named,
            source,
        }) => {
            assert_eq!(named, path);
            let message = source.to_string();
            assert!(message.contains("tasks.merge"), "{message}");
            assert!(message.contains("ask, checks, auto"), "{message}");
        }
        other => panic!("expected a refused file, got {other:?}"),
    }
}

#[test]
fn a_repo_committed_file_may_not_widen_a_guard() {
    let home = home();
    let repo = home.path().join("gated");
    std::fs::create_dir_all(repo.join(".shprd")).expect("the repo dir");
    let path = repo_file(&repo);
    for (document, key) in [
        ("[roles.worker]\nagent = \"codex\"\n", "roles"),
        ("[tasks]\nmerge = \"auto\"\n", "tasks.merge"),
        ("[tasks]\nmax_running = 10\n", "tasks.max_running"),
        ("[review]\ncross_vendor = false\n", "review"),
    ] {
        std::fs::write(&path, document).expect("the file is written");
        match read_repo_file(&repo) {
            Err(SettingsError::InFile {
                path: named,
                source,
            }) => {
                assert_eq!(named, path);
                let message = source.to_string();
                assert!(
                    message.contains(key),
                    "the refusal should name `{key}`, got: {message}"
                );
                assert!(
                    message.contains("cannot widen its own guards"),
                    "the refusal should say why: {message}"
                );
            }
            other => panic!("expected `{document}` to be refused, got {other:?}"),
        }
    }
    // The allow-listed keys load, and only them.
    std::fs::write(
        &path,
        "version = 1\n[tasks]\nbase = \"trunk\"\nbranch = \"task/{n}\"\nstart_notes = \"read AGENTS.md\"\n",
    )
    .expect("the file is written");
    let layer = read_repo_file(&repo).expect("the allow-listed file loads");
    assert_eq!(layer.len(), 3);
    assert_eq!(
        layer.get(&Key::Task(TaskKey::Base)),
        Some(&Value::Text("trunk".to_owned()))
    );
}

#[test]
fn the_old_config_keys_still_load() {
    let home = home();
    let path = user_config(home.path());
    std::fs::create_dir_all(path.parent().expect("the config dir")).expect("the dir");
    std::fs::write(
        &path,
        r#"
        mux = "herdr"

        [land]
        merge = "checks"

        [tick]
        max_running = 2
        interval_secs = 30

        [janitor]
        archive_days = 30
        namespace = "old/"

        [review]
        must_fix = "minor"
        rounds = 3
        "#,
    )
    .expect("the config is written");
    let loaded = Loaded::read(home.path(), None).expect("the old config loads");
    let resolved = loaded.resolve(None);
    let tasks = resolved.tasks();
    assert_eq!(tasks.merge, MergePolicy::Checks);
    assert_eq!(tasks.max_running, 2);
    assert_eq!(tasks.tick_secs, 30);
    assert_eq!(tasks.archive_after, 30);
    assert_eq!(tasks.archive_namespace, "old/");
    assert_eq!(tasks.must_fix, Severity::Minor);
    assert_eq!(tasks.review_rounds, 3);
    assert_eq!(
        source_of(&resolved, "tasks.merge"),
        Some(Source::Legacy),
        "an old key is supplied by the old config file, and says so"
    );
    assert_eq!(
        shown(&resolved, "tasks.proof"),
        "true",
        "what it never set stays the default"
    );

    // The settings file sits above the old file: the same key set in both
    // resolves to the settings file's value.
    let settings = user_file(home.path());
    std::fs::write(&settings, "[tasks]\nmax_running = 5\n").expect("the settings are written");
    let loaded = Loaded::read(home.path(), None).expect("both files load");
    let resolved = loaded.resolve(None);
    assert_eq!(resolved.tasks().max_running, 5);
    assert_eq!(
        resolved.tasks().merge,
        MergePolicy::Checks,
        "the keys the settings file leaves alone still come from the old file"
    );
}

#[test]
fn the_old_roster_reviewer_is_the_lower_layer_of_the_reviewer_role() {
    // A person who set the reviewer the pre-M9 way keeps that reviewer: the
    // role resolves from the old file, the source says so, and a settings value
    // set on top wins key by key.
    let home = home();
    let path = user_config(home.path());
    std::fs::create_dir_all(path.parent().expect("the config dir")).expect("the dir");
    std::fs::write(
        &path,
        r#"
        [roster]
        reviewer = "codex/gpt-5-codex"

        [roster.effort]
        "codex/gpt-5-codex" = "high"
        "#,
    )
    .expect("the old config is written");

    let loaded = Loaded::read(home.path(), None).expect("the old config loads");
    let resolved = loaded.resolve(None);
    let reviewer = resolved.role("reviewer").expect("the built-in reviewer");
    assert_eq!(
        reviewer.agent.as_ref().map(|a| a.value.as_str()),
        Some("codex")
    );
    assert_eq!(
        reviewer.model.as_ref().map(|m| m.value.as_str()),
        Some("gpt-5-codex")
    );
    assert_eq!(
        reviewer.effort.as_ref().map(|e| e.value.as_str()),
        Some("high")
    );
    assert_eq!(
        source_of(&resolved, "roles.reviewer.agent"),
        Some(Source::Legacy),
        "the reviewer's program comes from the old config file"
    );
    assert_eq!(
        source_of(&resolved, "roles.reviewer.model"),
        Some(Source::Legacy)
    );

    // The settings file sits above the old file, per key: setting the model
    // there keeps the old program and effort.
    let settings = user_file(home.path());
    std::fs::write(&settings, "[roles.reviewer]\nmodel = \"gpt-6\"\n")
        .expect("the settings are written");
    let loaded = Loaded::read(home.path(), None).expect("both files load");
    let resolved = loaded.resolve(None);
    let reviewer = resolved.role("reviewer").expect("the built-in reviewer");
    assert_eq!(
        reviewer.agent.as_ref().map(|a| a.value.as_str()),
        Some("codex"),
        "the program the old file set still applies"
    );
    assert_eq!(
        reviewer.model.as_ref().map(|m| m.value.as_str()),
        Some("gpt-6")
    );
    assert_eq!(
        source_of(&resolved, "roles.reviewer.model"),
        Some(Source::You),
        "the settings file is nearer than the old config file"
    );
    assert_eq!(
        source_of(&resolved, "roles.reviewer.effort"),
        Some(Source::Legacy)
    );
}

#[test]
fn keys_parse_from_their_names_and_their_old_names() {
    let cases = [
        ("tasks.merge", "tasks.merge"),
        ("land.merge", "tasks.merge"),
        ("tick.max_running", "tasks.max_running"),
        ("tick.interval_secs", "tasks.tick_secs"),
        ("janitor.archive_days", "tasks.archive_after"),
        ("janitor.namespace", "tasks.archive_namespace"),
        ("review.must_fix", "tasks.must_fix"),
        ("review.rounds", "tasks.review_rounds"),
        ("review.cross_vendor", "review.cross_vendor"),
        ("graph.dagr", "graph.dagr"),
        ("roles.order", "roles.order"),
        ("roles.worker.agent", "roles.worker.agent"),
        ("roles.docs_writer.fallback", "roles.docs_writer.fallback"),
        // Any id a key can hold is a role, either case included.
        ("roles.Docs.agent", "roles.Docs.agent"),
        ("roles.QA_2.kind", "roles.QA_2.kind"),
    ];
    for (written, canonical) in cases {
        let key = Key::parse(written).unwrap_or_else(|err| panic!("`{written}`: {err}"));
        assert_eq!(key.name(), canonical, "for `{written}`");
    }
    for refused in [
        "worker",
        "tasks.nope",
        "roles",
        "roles.order.x",
        "roles.worker.nope",
        "roles.my role.agent",
        "roles.docs/1.agent",
        "review",
        "agents.reactions",
        "graph",
        "graph.nope",
    ] {
        assert!(Key::parse(refused).is_err(), "`{refused}` must be refused");
    }
}

#[test]
fn the_graph_program_is_a_key_of_its_own_and_unset_by_default() {
    // Unset is what makes the graph block look for dagr itself: a value nobody
    // set must not be mistaken for a path.
    let resolved = resolve(&File::default(), &Request::default());
    assert_eq!(shown(&resolved, "graph.dagr"), "\"\"");
    assert_eq!(source_of(&resolved, "graph.dagr"), Some(Source::BuiltIn));
    assert_eq!(resolved.graph_dagr(), None, "an empty value is unset");

    let file = you("[graph]\ndagr = \"/opt/dagr/bin/dagr\"\n");
    let resolved = resolve(&file, &Request::default());
    assert_eq!(shown(&resolved, "graph.dagr"), "\"/opt/dagr/bin/dagr\"");
    assert_eq!(source_of(&resolved, "graph.dagr"), Some(Source::You));
    assert_eq!(resolved.graph_dagr(), Some("/opt/dagr/bin/dagr"));

    // A repository's committed file may not point the graph at a program: it is
    // written by whoever works in the repository, and this key runs one.
    let committed = "[graph]\ndagr = \"/tmp/dagr\"\n";
    match parse_repo_committed(committed) {
        Err(SettingsError::NotAllowed { key, .. }) => assert_eq!(key, "graph"),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_role_id_may_not_be_order_or_a_key_path() {
    for refused in ["roles.order.agent", "roles.a.b.c", "roles.not an id.agent"] {
        assert!(Key::parse(refused).is_err(), "`{refused}` must be refused");
    }
    // An id a dotted key can hold is a role, whatever its case.
    let key = Key::parse("roles.Upper.agent").expect("an uppercase id is a role");
    assert_eq!(key.name(), "roles.Upper.agent");
    let file = you("[roles.Docs]\nkind = \"worker\"\n");
    let resolved = resolve(&file, &Request::default());
    let docs = resolved.role("Docs").expect("the Docs role");
    assert_eq!(docs.kind.value, RoleKind::Worker);
    assert_eq!(docs.label(), "Docs");
    match Key::parse("roles.order.agent") {
        Err(SettingsError::BadValue { reason, .. }) => {
            assert!(reason.contains("roles.order"), "{reason}")
        }
        Err(SettingsError::UnknownKey { key, .. }) => assert_eq!(key, "roles.order.agent"),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_value_is_checked_against_the_key_that_takes_it() {
    let cases: [(&str, Value, bool); 8] = [
        ("tasks.merge", Value::Text("checks".to_owned()), true),
        ("tasks.merge", Value::Text("always".to_owned()), false),
        ("tasks.merge", Value::Switch(true), false),
        ("tasks.max_running", Value::Number(10), true),
        ("tasks.max_running", Value::Number(11), false),
        ("tasks.max_running", Value::Number(0), false),
        ("roles.worker.fast", Value::Switch(true), true),
        ("roles.worker.kind", Value::Text("boss".to_owned()), false),
    ];
    for (key, value, allowed) in cases {
        let key = Key::parse(key).expect("the key parses");
        let checked = key.check(&value);
        assert_eq!(
            checked.is_ok(),
            allowed,
            "`{}` = {}: {checked:?}",
            key.name(),
            value.display()
        );
    }
    // A refusal says what the key takes.
    let key = Key::parse("tasks.merge").expect("the key parses");
    let message = key
        .check(&Value::Text("always".to_owned()))
        .expect_err("a bad word is refused");
    assert!(message.contains("ask, checks, auto"), "{message}");
    let key = Key::parse("roles.order").expect("the key parses");
    assert!(
        key.check(&Value::List(vec!["docs".to_owned()])).is_ok(),
        "an id is a fine order item"
    );
    assert!(
        key.check(&Value::List(vec!["Not An Id".to_owned()]))
            .is_err()
    );
}

#[test]
fn a_roles_prompt_is_an_id_or_a_path_and_nothing_else() {
    let key = Key::parse("roles.docs.prompt").expect("the key parses");
    for good in [
        "prompt:docs",
        "prompt:worker",
        "~/prompts/docs.md",
        "/etc/shprd/docs.md",
        "./docs.md",
    ] {
        assert!(
            key.check(&Value::Text(good.to_owned())).is_ok(),
            "{good} names a prompt"
        );
    }
    for bad in ["prompt:", "", "   "] {
        let refused = key
            .check(&Value::Text(bad.to_owned()))
            .expect_err("a prompt nothing can be read from is refused");
        assert!(
            refused.contains("prompt:<id>") || refused.contains("neither"),
            "{bad}: {refused}"
        );
    }
}

#[test]
fn a_value_typed_on_the_command_line_parses_by_its_kind() {
    let key = Key::parse("tasks.max_running").expect("the key parses");
    assert_eq!(
        key.value_from_text("3").expect("a number"),
        Value::Number(3)
    );
    assert!(key.value_from_text("lots").is_err());
    let key = Key::parse("review.cross_vendor").expect("the key parses");
    assert_eq!(
        key.value_from_text("off").expect("a switch"),
        Value::Switch(false)
    );
    let key = Key::parse("roles.docs.fallback").expect("the key parses");
    assert_eq!(
        key.value_from_text("claude, pi").expect("a list"),
        Value::List(vec!["claude".to_owned(), "pi".to_owned()])
    );
    let key = Key::parse("roles.docs.agent").expect("the key parses");
    assert_eq!(
        key.value_from_text("codex").expect("text"),
        Value::Text("codex".to_owned())
    );
}

#[test]
fn a_file_with_another_version_is_refused() {
    match File::parse("version = 2\n") {
        Err(SettingsError::Version { found, expected }) => {
            assert_eq!(found, 2);
            assert_eq!(expected, VERSION);
        }
        other => panic!("expected a version refusal, got {other:?}"),
    }
}

#[test]
fn an_unknown_key_in_a_file_names_the_section_it_is_in() {
    let file = File::parse(
        r#"
        [repos.mine.tasks]
        merge = "checks"
        nope = true
        "#,
    );
    match file {
        Err(SettingsError::UnknownKey { key, .. }) => {
            assert_eq!(key, "repos.mine.tasks.nope")
        }
        other => panic!("expected an unknown key, got {other:?}"),
    }
}

#[test]
fn a_roles_order_list_is_reported_as_a_source() {
    let file = you("[roles]\norder = [\"docs\"]\n\n[roles.docs]\nkind = \"worker\"\n");
    let resolved = resolve(&file, &Request::default());
    assert_eq!(source_of(&resolved, "roles.order"), Some(Source::You));
    // The row shows the list that was set; the order a page draws is the
    // resolved one, which names every role.
    assert_eq!(shown(&resolved, "roles.order"), "[\"docs\"]");
    assert_eq!(
        resolved.order(),
        ["docs", "coordinator", "worker", "reviewer", "backup"]
    );
}

#[test]
fn the_host_setting_names_one_host_or_asks_the_machine() {
    assert_eq!(
        resolve(&File::default(), &Request::default()).host(),
        HostChoice::Auto,
        "nothing set: pick the host"
    );
    let file = you("host = \"herdr-desktop\"");
    let resolved = resolve(&file, &Request::default());
    assert_eq!(resolved.host(), HostChoice::HerdrDesktop);
    assert_eq!(source_of(&resolved, "host"), Some(Source::You));
    for word in HOST_WORDS {
        let file = you(&format!("host = {word:?}"));
        assert_eq!(
            resolve(&file, &Request::default()).host().name(),
            word,
            "{word} round-trips"
        );
    }
    // A word this build does not know is refused where it is read, naming the
    // ones it takes; a layer built by hand still resolves to `auto`, so a
    // caller holding an unknown word asks the machine instead of panicking.
    let refused = Key::parse("host").expect("the key parses");
    assert!(refused.check(&Value::Text("hologram".to_owned())).is_err());
    assert!(File::parse("host = \"hologram\"").is_err());
    assert_eq!(
        resolve(&File::default(), &Request::default()).host(),
        HostChoice::Auto
    );
    assert_eq!(
        refused.value_from_text("herdr").expect("a known host"),
        Value::Text("herdr".to_owned())
    );
}

#[test]
fn the_host_setting_is_written_to_the_persons_file() {
    let home = home();
    let loaded = Loaded::read(home.path(), None).expect("an empty home loads");
    let key = Key::parse("host").expect("the key parses");
    let written = loaded
        .write(
            home.path(),
            &Scope::You,
            &key,
            Some(Value::Text("herdr-desktop".to_owned())),
            None,
        )
        .expect("the host is stored");
    assert_eq!(written.resolved.host(), HostChoice::HerdrDesktop);
    let text = std::fs::read_to_string(user_file(home.path())).expect("the file exists");
    assert!(text.contains("host = \"herdr-desktop\""), "{text}");
}

#[test]
fn a_repo_name_is_the_folders_own_name() {
    assert_eq!(
        repo_name(Path::new("/home/me/projects/shprd")).as_deref(),
        Some("shprd")
    );
    assert_eq!(repo_name(Path::new("/")), None);
    assert_eq!(repo_name(Path::new(".")), None);
}

#[test]
fn a_value_round_trips_through_json() {
    for value in [
        Value::Text("checks".to_owned()),
        Value::Number(4),
        Value::Switch(true),
        Value::List(vec!["claude".to_owned(), "pi".to_owned()]),
    ] {
        assert_eq!(Value::from_json(&value.to_json()), Some(value));
    }
    assert_eq!(Value::from_json(&serde_json::json!(null)), None);
    assert_eq!(Value::from_json(&serde_json::json!(1.5)), None);
}

#[test]
fn the_web_keys_have_defaults_and_read_back_through_their_accessors() {
    let resolved = resolve(&File::default(), &Request::default());
    assert_eq!(shown(&resolved, "web.tunnel"), "\"tailscale\"");
    assert_eq!(shown(&resolved, "web.stable"), "false");
    assert_eq!(shown(&resolved, "web.cloudflared_name"), "\"\"");
    assert_eq!(shown(&resolved, "web.cloudflared_host"), "\"\"");
    assert_eq!(resolved.web_tunnel(), "tailscale");
    assert!(!resolved.web_stable());
    assert_eq!(resolved.web_cloudflared_name(), "");

    let file = you("[web]\ntunnel = \"cloudflared-named\"\nstable = true\n\
         cloudflared_name = \" shprd \"\ncloudflared_host = \"shprd.example.org\"\n");
    let resolved = resolve(&file, &Request::default());
    assert_eq!(resolved.web_tunnel(), "cloudflared-named");
    assert!(resolved.web_stable());
    assert_eq!(resolved.web_cloudflared_name(), "shprd");
    assert_eq!(resolved.web_cloudflared_host(), "shprd.example.org");
    assert_eq!(source_of(&resolved, "web.tunnel"), Some(Source::You));
}

#[test]
fn web_tunnel_takes_only_the_provider_words() {
    let key = Key::parse("web.tunnel").expect("the key parses");
    for word in WEB_TUNNEL_WORDS {
        assert!(key.value_from_text(word).is_ok(), "{word}");
    }
    assert!(key.value_from_text("carrier-pigeon").is_err());
    assert!(File::parse("[web]\ntunnel = \"carrier-pigeon\"\n").is_err());
    assert!(File::parse("[web]\nnonsense = 1\n").is_err());
    assert!(Key::parse("web.nonsense").is_err());
    assert!(Key::parse("web").is_err());
}

#[test]
fn a_repository_may_not_choose_where_a_shell_is_exposed() {
    for key in WEB_KEYS {
        assert!(!Key::Web(key).is_repo_allowed(), "{}", key.name());
    }
    let repo = parse_repo_committed("[web]\ntunnel = \"ngrok\"\n");
    assert!(
        repo.is_err(),
        "a committed [web] table is refused: {repo:?}"
    );
}

#[test]
fn a_web_key_is_written_into_the_web_table() {
    let home = home();
    let loaded = Loaded::read(home.path(), None).expect("an empty home loads");
    let key = Key::parse("web.tunnel").expect("the key parses");
    let written = loaded
        .write(
            home.path(),
            &Scope::You,
            &key,
            Some(Value::Text("tunnl".to_owned())),
            None,
        )
        .expect("the tunnel is stored");
    assert_eq!(written.resolved.web_tunnel(), "tunnl");
    let text = std::fs::read_to_string(user_file(home.path())).expect("the file was written");
    assert!(
        text.contains("[web]") && text.contains("tunnel = \"tunnl\""),
        "{text}"
    );
}
