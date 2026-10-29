//! Archive refs, bundles and restore, against real git.

mod support;

use shprd_git::archive::{self, ArchiveError, archive_ref_for_attempt, archive_ref_name};

use support::Repo;

/// The attempt-1 archive ref the tests archive `shprd/r1/T1` under.
fn attempt_ref(attempt: u32) -> String {
    archive_ref_for_attempt("r1", "T1", attempt)
}

#[test]
fn archive_ref_name_is_the_documented_shape() {
    assert_eq!(archive_ref_name("m1", "M1.2"), "refs/shprd/archive/m1/M1.2");
    assert!(archive::is_archive_ref(&archive_ref_name("m1", "M1.2")));
    assert!(!archive::is_archive_ref("refs/heads/shprd/m1/M1.2"));
}

#[test]
fn archive_then_restore_round_trips_the_same_commit() {
    let repo = Repo::new();
    let layout = repo.layout();
    repo.git(&["branch", "shprd/r1/T1", "main"]);
    let tip = repo.commit("work.txt", "work\n", "work");
    repo.git(&["branch", "-f", "shprd/r1/T1", &tip]);

    let ref_name = attempt_ref(1);
    let archived = match archive::archive_to(&repo.path, "shprd/r1/T1", &ref_name) {
        Ok(archived) => archived,
        Err(err) => panic!("archive must succeed: {err}"),
    };
    assert_eq!(archived.ref_name, ref_name);
    assert_eq!(archived.sha, tip);

    repo.git(&["branch", "-D", "shprd/r1/T1"]);
    let branch = match archive::restore(&repo.path, &layout, "r1", "T1") {
        Ok(branch) => branch,
        Err(err) => panic!("restore must succeed: {err}"),
    };
    assert_eq!(branch, "shprd/r1/T1");
    assert_eq!(repo.git(&["rev-parse", "shprd/r1/T1"]), tip);
}

#[test]
fn archiving_the_same_commit_twice_is_a_no_op() {
    let repo = Repo::new();
    repo.git(&["branch", "shprd/r1/T1", "main"]);
    let ref_name = attempt_ref(1);
    let first = match archive::archive_to(&repo.path, "shprd/r1/T1", &ref_name) {
        Ok(archived) => archived,
        Err(err) => panic!("archive must succeed: {err}"),
    };
    let second = match archive::archive_to(&repo.path, "shprd/r1/T1", &ref_name) {
        Ok(archived) => archived,
        Err(err) => panic!("a repeated archive must succeed: {err}"),
    };
    assert_eq!(first, second);
}

#[test]
fn archive_refuses_to_move_an_existing_ref_to_a_different_commit() {
    let repo = Repo::new();
    repo.git(&["branch", "shprd/r1/T1", "main"]);
    let first = repo.head_sha();

    let ref_name = attempt_ref(1);
    let older = match archive::archive_to(&repo.path, "shprd/r1/T1", &ref_name) {
        Ok(archived) => archived,
        Err(err) => panic!("archiving the seed commit must succeed: {err}"),
    };
    let moved = repo.commit_empty("second");
    repo.git(&["branch", "-f", "shprd/r1/T1", &moved]);
    let err = match archive::archive_to(&repo.path, "shprd/r1/T1", &ref_name) {
        Ok(archived) => panic!("a moved archive ref must be refused, got {archived:?}"),
        Err(err) => err,
    };
    assert!(matches!(err, ArchiveError::ArchiveExists { .. }), "{err:?}");
    assert_eq!(
        repo.git(&["rev-parse", &older.ref_name]),
        first,
        "the ref must not move"
    );
}

#[test]
fn restore_refuses_a_branch_that_already_exists() {
    let repo = Repo::new();
    let layout = repo.layout();
    repo.git(&["branch", "shprd/r1/T1", "main"]);
    let ref_name = attempt_ref(1);
    if let Err(err) = archive::archive_to(&repo.path, "shprd/r1/T1", &ref_name) {
        panic!("archive must succeed: {err}");
    }
    let err = match archive::restore(&repo.path, &layout, "r1", "T1") {
        Ok(branch) => panic!("an existing branch must be refused, got {branch:?}"),
        Err(err) => err,
    };
    assert!(matches!(err, ArchiveError::BranchExists { .. }), "{err:?}");
}

#[test]
fn restore_refuses_when_nothing_was_archived() {
    let repo = Repo::new();
    let layout = repo.layout();
    let err = match archive::restore(&repo.path, &layout, "r1", "T9") {
        Ok(branch) => panic!("a missing archive must be refused, got {branch:?}"),
        Err(err) => err,
    };
    assert!(
        matches!(err, ArchiveError::MissingArchive { .. }),
        "{err:?}"
    );
}

#[test]
fn bundle_writes_the_range_and_skips_an_empty_one() {
    let repo = Repo::new();
    repo.git(&["branch", "shprd/r1/T1", "main"]);
    let out = repo.path.join("out/t1.bundle");

    let empty = match archive::bundle(&repo.path, "main", "shprd/r1/T1", &out) {
        Ok(path) => path,
        Err(err) => panic!("bundle must succeed: {err}"),
    };
    assert_eq!(empty, None, "an empty range must write no file");
    assert!(!out.exists(), "no bundle file may be left behind");

    repo.commit("work.txt", "work\n", "work");
    repo.git(&["branch", "-f", "shprd/r1/T1", "main"]);
    let written = match archive::bundle(&repo.path, "main~1", "shprd/r1/T1", &out) {
        Ok(path) => path,
        Err(err) => panic!("bundle must succeed: {err}"),
    };
    assert_eq!(written, Some(out.clone()));
    assert!(out.is_file(), "{} must be a file", out.display());
}

#[test]
fn an_archive_ref_must_be_an_archive_ref() {
    let repo = Repo::new();
    if let Err(err) = archive::expire(&repo.path, "refs/heads/main") {
        assert!(matches!(err, ArchiveError::InvalidId { .. }), "{err:?}");
    } else {
        panic!("expiring a branch ref must be refused");
    }
    assert!(!repo.git(&["branch", "--list", "main"]).is_empty());

    let layout = repo.layout();
    for branch in ["main", "shprd/", "shprd/../escape", "other/r1/T1"] {
        if let Err(err) = archive::archive_ref_for_branch(&layout, branch) {
            assert!(matches!(err, ArchiveError::InvalidId { .. }), "{err:?}");
        } else {
            panic!("{branch:?} must be refused");
        }
    }
    assert_eq!(
        archive::archive_ref_for_branch(&layout, "shprd/r1/T1-retry").ok(),
        Some("refs/shprd/archive/r1/T1-retry".to_string()),
        "an extra attempt names its own ref"
    );
}

#[test]
fn each_attempt_archives_to_its_own_ref() {
    let repo = Repo::new();
    let first_ref = archive_ref_for_attempt("r1", "T1", 1);
    let second_ref = archive_ref_for_attempt("r1", "T1", 2);
    let third_ref = archive_ref_for_attempt("r1", "T1", 3);
    assert_eq!(first_ref, "refs/shprd/archive/r1/T1-a1");
    assert_ne!(
        first_ref,
        archive_ref_name("r1", "T1"),
        "no attempt uses the plain task ref"
    );
    assert_ne!(first_ref, second_ref);
    assert_ne!(second_ref, third_ref);

    repo.git(&["branch", "shprd/r1/T1", "main"]);
    let first = repo.commit_empty("attempt one");
    repo.git(&["branch", "-f", "shprd/r1/T1", &first]);
    let kept = match archive::archive_to(&repo.path, "shprd/r1/T1", &first_ref) {
        Ok(archived) => archived,
        Err(err) => panic!("archiving attempt 1 must succeed: {err}"),
    };
    assert_eq!(kept.sha, first);

    let second = repo.commit_empty("attempt two");
    repo.git(&["branch", "-f", "shprd/r1/T1", &second]);
    match archive::archive_to(&repo.path, "shprd/r1/T1", &second_ref) {
        Ok(archived) => assert_eq!(archived.sha, second),
        Err(err) => panic!("archiving attempt 2 must succeed: {err}"),
    }
    // Neither attempt's commits are lost: each ref still holds its own.
    assert_eq!(repo.git(&["rev-parse", &first_ref]), first);
    assert_eq!(repo.git(&["rev-parse", &second_ref]), second);
}

#[test]
fn archive_to_refuses_a_ref_outside_the_archive_namespace() {
    let repo = Repo::new();
    repo.git(&["branch", "shprd/r1/T1", "main"]);
    let err = match archive::archive_to(&repo.path, "shprd/r1/T1", "refs/heads/shprd/kept") {
        Ok(archived) => panic!("a ref outside the archive namespace must be refused: {archived:?}"),
        Err(err) => err,
    };
    assert!(matches!(err, ArchiveError::InvalidId { .. }), "{err:?}");
}

#[test]
fn restore_picks_the_newest_attempt_archive() {
    let repo = Repo::new();
    let layout = repo.layout();
    repo.git(&["branch", "shprd/r1/T1", "main"]);
    let first = repo.commit_empty("attempt one");
    repo.git(&["branch", "-f", "shprd/r1/T1", &first]);
    let first_ref = attempt_ref(1);
    if let Err(err) = archive::archive_to(&repo.path, "shprd/r1/T1", &first_ref) {
        panic!("archiving attempt 1 must succeed: {err}");
    }
    // Attempt 3 replaces the tip on the same branch, as a retry does.
    let newest = repo.commit_empty("attempt three");
    repo.git(&["branch", "-f", "shprd/r1/T1", &newest]);
    let newest_ref = attempt_ref(3);
    if let Err(err) = archive::archive_to(&repo.path, "shprd/r1/T1", &newest_ref) {
        panic!("archiving attempt 3 must succeed: {err}");
    }
    assert_eq!(repo.git(&["rev-parse", &first_ref]), first);
    assert_eq!(repo.git(&["rev-parse", &newest_ref]), newest);

    repo.git(&["branch", "-D", "shprd/r1/T1"]);
    let branch = match archive::restore(&repo.path, &layout, "r1", "T1") {
        Ok(branch) => branch,
        Err(err) => panic!("restore must succeed: {err}"),
    };
    assert_eq!(branch, "shprd/r1/T1");
    assert_eq!(
        repo.git(&["rev-parse", "shprd/r1/T1"]),
        newest,
        "the highest attempt, not the first, comes back"
    );
}

#[test]
fn restore_falls_back_to_a_legacy_plain_archive() {
    let repo = Repo::new();
    let layout = repo.layout();
    repo.git(&["branch", "shprd/r1/T1", "main"]);
    let legacy = repo.commit_empty("archived before per-attempt refs");
    repo.git(&["branch", "-f", "shprd/r1/T1", &legacy]);
    // An archive written by an earlier shprd: the plain task ref, no attempt.
    let plain = archive_ref_name("r1", "T1");
    repo.git(&["update-ref", &plain, &legacy]);
    repo.git(&["branch", "-D", "shprd/r1/T1"]);

    let branch = match archive::restore(&repo.path, &layout, "r1", "T1") {
        Ok(branch) => branch,
        Err(err) => panic!("a legacy archive must still restore: {err}"),
    };
    assert_eq!(branch, "shprd/r1/T1");
    assert_eq!(repo.git(&["rev-parse", "shprd/r1/T1"]), legacy);
}
