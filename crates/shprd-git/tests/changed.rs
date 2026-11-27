//! Changed-path detection and the protected-path flag, against real git.

mod support;

use shprd_git::changed::{changed_paths, protected_touches, task_changes};

use support::Repo;

/// A repo with a `topic` branch that edited, added, deleted and renamed files.
fn branched_repo() -> Repo {
    let repo = Repo::new();
    repo.commit("edited.txt", "one\n", "add edited");
    repo.commit("deleted.txt", "gone\n", "add deleted");
    repo.commit("old.txt", "rename me\n", "add old");
    repo.git(&["checkout", "-b", "topic"]);
    repo.commit("edited.txt", "two\n", "edit");
    repo.commit("added.txt", "new\n", "add");
    repo.git(&["rm", "--quiet", "deleted.txt"]);
    repo.git(&["mv", "old.txt", "new.txt"]);
    repo.git(&["commit", "-m", "delete and rename"]);
    repo
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

#[test]
fn lists_edited_added_deleted_and_both_sides_of_a_rename() {
    let repo = branched_repo();
    let paths = match changed_paths(&repo.path, "main", "topic") {
        Ok(paths) => paths,
        Err(err) => panic!("diff must succeed: {err}"),
    };
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        strings(&[
            "added.txt",
            "deleted.txt",
            "edited.txt",
            "new.txt",
            "old.txt",
        ]),
    );
}

#[test]
fn an_empty_range_has_no_changed_paths() {
    let repo = Repo::new();
    let sha = repo.head_sha();
    let paths = match changed_paths(&repo.path, &sha, &sha) {
        Ok(paths) => paths,
        Err(err) => panic!("diff must succeed: {err}"),
    };
    assert!(paths.is_empty(), "{paths:?}");
}

/// Cuts `name` from `main`, lets `main` gain 120 files, then merges `main` into
/// the branch and edits two files of its own.
fn branch_that_merged_a_moved_main(repo: &Repo, name: &str, own: [&str; 2]) {
    repo.git(&["checkout", "-b", name, "main"]);
    repo.git(&["checkout", "main"]);
    for n in 0..120 {
        repo.commit(
            &format!("gained/f{n}.txt"),
            "x\n",
            &format!("main gains {n}"),
        );
    }
    repo.git(&["checkout", name]);
    repo.git(&["merge", "--no-edit", "main"]);
    for path in own {
        repo.commit(path, &format!("{name}\n"), &format!("{name}: {path}"));
    }
    repo.git(&["checkout", "main"]);
}

#[test]
fn a_task_that_merged_a_moved_main_changed_only_its_own_files() {
    let repo = Repo::new();
    let cut = repo.head_sha();
    branch_that_merged_a_moved_main(&repo, "t1", ["src/a.rs", "src/b.rs"]);
    let stale = match changed_paths(&repo.path, &cut, "t1") {
        Ok(paths) => paths,
        Err(err) => panic!("diff: {err}"),
    };
    assert!(stale.len() > 100, "from the cut commit main's files count");
    let paths = match task_changes(&repo.path, "main", "t1") {
        Ok(paths) => paths,
        Err(err) => panic!("task changes: {err}"),
    };
    let mut sorted = paths;
    sorted.sort();
    assert_eq!(sorted, strings(&["src/a.rs", "src/b.rs"]));
}

#[test]
fn two_tasks_that_each_merged_the_new_main_do_not_blame_each_other() {
    let repo = Repo::new();
    repo.git(&["branch", "t3"]);
    repo.git(&["branch", "t4"]);
    for n in 0..110 {
        repo.commit(
            &format!("gained/g{n}.txt"),
            "x\n",
            &format!("main gains {n}"),
        );
    }
    for (name, own) in [
        ("t3", ["a/one.rs", "a/two.rs"]),
        ("t4", ["b/one.rs", "b/two.rs"]),
    ] {
        repo.git(&["checkout", name]);
        repo.git(&["merge", "--no-edit", "main"]);
        for path in own {
            repo.commit(path, name, &format!("{name}: {path}"));
        }
    }
    repo.git(&["checkout", "main"]);
    for (name, scope) in [("t3", "a"), ("t4", "b")] {
        let paths = match task_changes(&repo.path, "main", name) {
            Ok(paths) => paths,
            Err(err) => panic!("task changes: {err}"),
        };
        assert_eq!(paths.len(), 2, "{name} changed two files");
        assert!(
            paths
                .iter()
                .all(|path| path.starts_with(&format!("{scope}/"))),
            "{name}: {paths:?}"
        );
    }
}

#[test]
fn a_path_with_the_same_content_as_the_base_is_not_the_tasks_change() {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "t1"]);
    repo.commit("same.txt", "same\n", "task writes same");
    repo.commit("mine.txt", "mine\n", "task writes mine");
    repo.git(&["checkout", "main"]);
    // main lands the same file independently (a squash), so the merge base lags.
    repo.commit("same.txt", "same\n", "main writes same");
    let paths = match task_changes(&repo.path, "main", "t1") {
        Ok(paths) => paths,
        Err(err) => panic!("task changes: {err}"),
    };
    assert_eq!(paths, strings(&["mine.txt"]));
}

#[test]
fn the_repos_own_list_and_the_built_ins_flag_undeclared_protected_paths() {
    let repo = Repo::new();
    repo.commit(
        ".agents/orchestration.yaml",
        "high_risk_paths:\n  - docs/\n",
        "protect docs",
    );
    repo.git(&["checkout", "-b", "t1"]);
    repo.commit("src/a.rs", "a\n", "work");
    repo.commit("docs/plan.md", "plan\n", "docs");
    repo.commit("AGENTS.md", "rules\n", "rules");
    repo.git(&["checkout", "main"]);
    let flagged = match protected_touches(&repo.path, "main", "t1", &strings(&["src"])) {
        Ok(paths) => paths,
        Err(err) => panic!("protected touches: {err}"),
    };
    assert_eq!(flagged, strings(&["AGENTS.md", "docs/plan.md"]));
    let declared = match protected_touches(&repo.path, "main", "t1", &strings(&["src", "docs"])) {
        Ok(paths) => paths,
        Err(err) => panic!("protected touches: {err}"),
    };
    assert_eq!(
        declared,
        strings(&["AGENTS.md"]),
        "a declared path is not flagged"
    );
}
