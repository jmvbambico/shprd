//! Archive refs, bundles and restore, against real git.

mod support;

use shprd_git::archive::{self, ArchiveError, archive_ref_name};

use support::Repo;

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

    let archived = match archive::archive(&repo.path, &layout, "shprd/r1/T1") {
        Ok(archived) => archived,
        Err(err) => panic!("archive must succeed: {err}"),
    };
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
    let layout = repo.layout();
    repo.git(&["branch", "shprd/r1/T1", "main"]);
    let first = match archive::archive(&repo.path, &layout, "shprd/r1/T1") {
        Ok(archived) => archived,
        Err(err) => panic!("archive must succeed: {err}"),
    };
    let second = match archive::archive(&repo.path, &layout, "shprd/r1/T1") {
        Ok(archived) => archived,
        Err(err) => panic!("a repeated archive must succeed: {err}"),
    };
    assert_eq!(first, second);
}

#[test]
fn archive_refuses_to_move_an_existing_ref_to_a_different_commit() {
    let repo = Repo::new();
    let layout = repo.layout();
    repo.git(&["branch", "shprd/r1/T1", "main"]);
    let first = repo.head_sha();

    let older = match archive::archive(&repo.path, &layout, "shprd/r1/T1") {
        Ok(archived) => archived,
        Err(err) => panic!("archiving the seed commit must succeed: {err}"),
    };
    let moved = repo.commit_empty("second");
    repo.git(&["branch", "-f", "shprd/r1/T1", &moved]);
    let err = match archive::archive(&repo.path, &layout, "shprd/r1/T1") {
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
    if let Err(err) = archive::archive(&repo.path, &layout, "shprd/r1/T1") {
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
