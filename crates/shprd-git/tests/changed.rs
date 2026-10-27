//! Changed-path detection and scope checking, against real git.

mod support;

use shprd_git::changed::{changed_paths, outside_scope};

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

#[test]
fn flags_a_path_outside_the_scope_and_accepts_one_inside() {
    let scope = strings(&["crates/shprd-git"]);
    let inside = strings(&["crates/shprd-git/src/lib.rs", "./crates/shprd-git/"]);
    let outside = strings(&["docs/plan.md", "../escape"]);
    assert!(outside_scope(&inside, &scope).is_empty());
    assert_eq!(outside_scope(&outside, &scope), outside);
}
