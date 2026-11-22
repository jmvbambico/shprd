//! The transcript readers, over recorded fixture sessions of all four programs.
//!
//! Each fixture is a real session's shape, copied from this machine and then
//! scrubbed: every personal path is `/work/repo` or `/home/you`, every account,
//! session and message id is a fixture id, and the assistant's words are sample
//! words rather than anything a person wrote. The harness table is the one that
//! ships (`data/harness/*.toml`), so a reader is tested against the same
//! `[transcript]` line a release reads.
//!
//! The tests copy a fixture's home into a temp directory before they append to
//! it: a recorded session is evidence and is never written to, and a test that
//! grows a record is exactly how the resumed-offset and half-written-line rules
//! are proved.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use shprd_agents::harness::Harness;
use shprd_agents::spec::TranscriptFormat;
use shprd_agents::transcript::{Role, Transcript, TranscriptError, cwd_slug};

/// The working directory every fixture session records.
const CWD: &str = "/work/repo";

/// The directory the recorded sessions live under.
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("transcripts")
}

/// The harness `program`'s table entry.
fn harness(program: &str) -> Harness {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/harness")
        .join(format!("{program}.toml"));
    Harness::load(&path).expect("the shipped harness loads")
}

/// A temp copy of one program's fixture home, so a test may append to it.
fn recorded(program: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("a temp home");
    copy_dir(&fixtures().join(program).join("home"), dir.path());
    let home = dir.path().to_path_buf();
    // OpenCode's fixture is a set of rows rather than a database file, so a
    // review can read it: the database is built from them here, exactly as
    // OpenCode's own schema stores them.
    let rows = home.join(".local/share/opencode/rows");
    if rows.is_dir() {
        build_opencode(&rows, &home.join(".local/share/opencode/opencode.db"));
        fs::remove_dir_all(&rows).unwrap();
    }
    (dir, home)
}

/// Build an OpenCode database from `rows`, the way OpenCode 1.18.35 stores it.
///
/// One `session` row names the directory; `message` holds one row a message with
/// the header in `data`, and `part` one row a part. The fixture keeps the rows as
/// JSON files so a person reviewing the diff can read them.
fn build_opencode(rows: &Path, db: &Path) {
    let connection = rusqlite::Connection::open(db).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT NOT NULL, vcs TEXT,
                name TEXT, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
             CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT NOT NULL,
                parent_id TEXT, slug TEXT NOT NULL, directory TEXT NOT NULL,
                title TEXT NOT NULL, version TEXT NOT NULL, time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL,
                session_id TEXT NOT NULL, time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL, data TEXT NOT NULL);",
        )
        .unwrap();
    let read = |path: &Path| -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    };
    for file in fs::read_dir(rows.join("session")).unwrap().flatten() {
        let session = read(&file.path());
        connection
            .execute(
                "INSERT INTO project VALUES (?1, ?2, 'git', 'repo', ?3, ?3)",
                (
                    session["project_id"].as_str().unwrap(),
                    session["directory"].as_str().unwrap(),
                    session["time_created"].as_i64().unwrap(),
                ),
            )
            .ok();
        connection
            .execute(
                "INSERT INTO session VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8)",
                (
                    session["id"].as_str().unwrap(),
                    session["project_id"].as_str().unwrap(),
                    session["slug"].as_str().unwrap(),
                    session["directory"].as_str().unwrap(),
                    session["title"].as_str().unwrap(),
                    session["version"].as_str().unwrap(),
                    session["time_created"].as_i64().unwrap(),
                    session["time_updated"].as_i64().unwrap(),
                ),
            )
            .unwrap();
    }
    // One folder a kind, one file a row, named by the session the row belongs
    // to: the same shape OpenCode's own store used before the database.
    for (kind, sql) in [
        ("message", "INSERT INTO message VALUES (?1, ?2, ?3, ?3, ?4)"),
        ("part", "INSERT INTO part VALUES (?1, ?2, ?3, ?4, ?4, ?5)"),
    ] {
        for session in fs::read_dir(rows.join(kind)).unwrap().flatten() {
            for file in fs::read_dir(session.path()).unwrap().flatten() {
                let row = read(&file.path());
                match kind {
                    "message" => connection
                        .execute(
                            sql,
                            (
                                row["id"].as_str().unwrap(),
                                row["session_id"].as_str().unwrap(),
                                row["time_created"].as_i64().unwrap(),
                                row["data"].to_string(),
                            ),
                        )
                        .unwrap(),
                    _ => connection
                        .execute(
                            sql,
                            (
                                row["id"].as_str().unwrap(),
                                row["message_id"].as_str().unwrap(),
                                row["session_id"].as_str().unwrap(),
                                row["time_created"].as_i64().unwrap(),
                                row["data"].to_string(),
                            ),
                        )
                        .unwrap(),
                };
            }
        }
    }
}

/// Locate the fixture session of `program` under `home`.
fn locate(program: &str, home: &Path) -> Transcript {
    Transcript::locate(harness(program).transcript(), home, Path::new(CWD))
        .unwrap_or_else(|| panic!("{program}'s fixture session is found"))
}

/// Copy `from` into `to`, depth first.
fn copy_dir(from: &Path, to: &Path) {
    for entry in fs::read_dir(from)
        .expect("the fixture home reads")
        .flatten()
    {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            fs::create_dir_all(&target).unwrap();
            copy_dir(&entry.path(), &target);
        } else {
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Append `line` to `path`, exactly as a CLI appending its record would.
fn append_line(path: &Path, line: &str) {
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{line}").unwrap();
}

#[test]
fn claude_reads_the_persons_turn_the_agents_reply_and_its_tool_as_one_conversation() {
    let (_dir, home) = recorded("claude");
    let transcript = locate("claude", &home);
    assert_eq!(transcript.session(), "5f0c-session");
    let page = transcript.read("").expect("the fixture reads");
    assert!(!page.more, "one short session is one page");
    // The four lines of the file that are messages: the slash command the
    // person ran, the question they pasted, and the agent's two replies.
    assert_eq!(page.messages.len(), 4, "{:?}", page.messages);
    assert_eq!(page.messages[0].role, Role::User);
    assert_eq!(page.messages[0].text, "/login");
    assert_eq!(page.messages[1].role, Role::User);
    assert_eq!(
        page.messages[1].text,
        "Read the run log and tell me what the last pass did."
    );
    assert!(page.messages[1].tools.is_empty());
    assert_eq!(page.messages[2].role, Role::Assistant);
    assert_eq!(page.messages[2].text, "I will read the tail of the log.");
    assert_eq!(page.messages[2].tools[0].name, "Bash");
    assert_eq!(page.messages[2].tools[0].summary, "shprd tick --once");
    assert_eq!(page.messages[3].role, Role::Assistant);
    assert_eq!(
        page.messages[3].text,
        "The last pass started T2 and landed T1."
    );
    assert!(page.messages[3].at_ms.is_some());
    // The CLI's own lines and the subagent's sidechain are in the file and are
    // not messages: the person did not say them and did not read them.
    let text = format!("{:?}", page.messages);
    for injected in [
        "Caveat: The messages below",
        "Login successful",
        "command-message",
        "pasted_content",
        "A subagent's own prompt.",
        "tool_result",
    ] {
        assert!(!text.contains(injected), "{injected:?} was shown: {text}");
    }
}

#[test]
fn a_half_written_last_line_is_read_once_it_is_complete() {
    let (_dir, home) = recorded("claude");
    let transcript = locate("claude", &home);
    let path = transcript.path().expect("a file format").to_path_buf();
    // The fixture's last line is an agent's answer with no trailing newline.
    // Cut it in half, which is what a program appending its record looks like
    // to a reader who arrives at the wrong moment: half a JSON object is not a
    // message, so it waits.
    let text = fs::read_to_string(&path).unwrap();
    let start = text.rfind('\n').map(|at| at + 1).unwrap_or(0);
    let last = &text[start..];
    assert!(last.starts_with('{'), "the fixture ends in its last line");
    let half = last.len() / 2;
    fs::write(&path, format!("{}{}", &text[..start], &last[..half])).unwrap();
    let page = transcript.read("").expect("the fixture reads");
    assert_eq!(page.messages.len(), 3, "the half-written line is not read");
    // The program finishes writing it, and the page picks up the rest.
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(format!("{}\n", &last[half..]).as_bytes())
        .unwrap();
    let page = transcript.read(&page.cursor).expect("the fixture reads");
    assert_eq!(page.messages.len(), 1);
    assert_eq!(
        page.messages[0].text,
        "The last pass started T2 and landed T1."
    );
    // A resumed cursor with nothing appended is an empty page, and stays one.
    let again = transcript.read(&page.cursor).expect("the fixture reads");
    assert!(again.messages.is_empty());
    assert_eq!(again.cursor, page.cursor);
}

#[test]
fn a_resumed_offset_reads_only_what_the_record_gained() {
    let (_dir, home) = recorded("claude");
    let transcript = locate("claude", &home);
    let first = transcript.read("").expect("the fixture reads");
    append_line(
        transcript.path().unwrap(),
        r#"{"type":"user","isSidechain":false,"timestamp":"2026-11-04T10:01:00.000Z","message":{"role":"user","content":"Now land it."}}"#,
    );
    let page = transcript.read(&first.cursor).expect("the fixture reads");
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].text, "Now land it.");
    assert_eq!(page.messages[0].role, Role::User);
}

#[test]
fn a_cursor_from_another_session_starts_again() {
    let (_dir, home) = recorded("pi");
    let transcript = locate("pi", &home);
    let page = transcript.read("").expect("the fixture reads");
    let strays = transcript
        .read("some-other-session@999")
        .expect("the fixture reads");
    assert_eq!(strays.messages.len(), page.messages.len());
    assert!(!strays.messages.is_empty());
}

#[test]
fn a_cursor_past_the_end_of_a_shorter_file_starts_again() {
    let (_dir, home) = recorded("claude");
    let transcript = locate("claude", &home);
    let page = transcript
        .read("5f0c-session@9999999")
        .expect("the fixture reads");
    assert_eq!(
        page.messages.len(),
        4,
        "a truncated record is read from its start"
    );
}

#[test]
fn pi_reads_its_reply_with_the_call_summarised() {
    let (_dir, home) = recorded("pi");
    let transcript = locate("pi", &home);
    assert_eq!(transcript.session(), "2026-11-14T00-39-35-049Z_sw-fixture");
    let page = transcript.read("").expect("the fixture reads");
    assert_eq!(page.messages.len(), 2);
    assert_eq!(page.messages[0].role, Role::User);
    assert_eq!(
        page.messages[0].text,
        "Start task T3 and tell me how it goes."
    );
    assert_eq!(page.messages[1].role, Role::Assistant);
    assert_eq!(page.messages[1].text, "Starting T3 now.");
    assert_eq!(page.messages[1].tools.len(), 1);
    assert_eq!(page.messages[1].tools[0].name, "bash");
    assert_eq!(
        page.messages[1].tools[0].summary,
        "shprd agent spawn --task T3"
    );
    assert_eq!(page.messages[1].at_ms, Some(1_794_616_785_000));
}

#[test]
fn codex_skips_its_own_brief_and_reads_the_review() {
    let (_dir, home) = recorded("codex");
    let transcript = locate("codex", &home);
    let page = transcript.read("").expect("the fixture reads");
    // Three messages: the person's one turn (with the environment block beside
    // their words dropped), the agent's reply, and its tool call.
    assert_eq!(page.messages.len(), 3, "{:?}", page.messages);
    assert_eq!(page.messages[0].role, Role::User);
    assert_eq!(page.messages[0].text, "Review the diff on branch work/12.");
    assert_eq!(page.messages[1].role, Role::Assistant);
    assert_eq!(page.messages[1].text, "I will read the diff first.");
    assert_eq!(page.messages[2].tools.len(), 1);
    assert_eq!(page.messages[2].tools[0].name, "exec");
    let shown = format!("{:?}", page.messages);
    for injected in [
        "You are Codex.",
        "AGENTS.md instructions",
        "<environment_context>",
        "<INSTRUCTIONS>",
    ] {
        assert!(
            !shown.contains(injected),
            "{injected:?} is the program's own brief, not the conversation: {shown}"
        );
    }
}

#[test]
fn codex_finds_the_rollout_that_names_this_directory() {
    let (_dir, home) = recorded("codex");
    // A rollout for another directory, newer than the fixture's, must not win.
    let dir = home.join(".codex/sessions/2026/11/04");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("rollout-2026-11-04T00-00-00-elsewhere.jsonl"),
        concat!(
            r#"{"type":"session_meta","payload":{"cwd":"/work/other"}}"#,
            "\n",
            r#"{"type":"response_item","timestamp":"2026-11-04T00:00:01.000Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Not this repository."}]}}"#,
            "\n"
        ),
    )
    .unwrap();
    let transcript = locate("codex", &home);
    let page = transcript.read("").expect("the fixture reads");
    assert_eq!(page.messages[0].text, "Review the diff on branch work/12.");
}

#[test]
fn opencode_joins_the_messages_and_parts_of_its_database() {
    let (_dir, home) = recorded("opencode");
    let transcript = locate("opencode", &home);
    assert_eq!(transcript.session(), "ses_fixture");
    assert_eq!(transcript.format(), TranscriptFormat::OpencodeSqlite);
    let page = transcript.read("").expect("the fixture reads");
    assert_eq!(page.messages.len(), 2);
    assert_eq!(page.messages[0].role, Role::User);
    assert_eq!(page.messages[0].text, "Review the last commit.");
    assert_eq!(page.messages[1].role, Role::Assistant);
    assert_eq!(page.messages[1].text, "Reading the commit now.");
    assert_eq!(
        page.messages[1].tools.len(),
        1,
        "the reasoning part is bookkeeping, the tool is a row"
    );
    assert_eq!(page.messages[1].tools[0].name, "bash");
    assert_eq!(page.messages[1].tools[0].summary, "git show --stat HEAD");
    assert!(page.messages[1].at_ms.is_some());
    assert_eq!(page.cursor, "ses_fixture@2");
}

#[test]
fn opencode_reads_on_from_the_message_index_and_only_finds_its_own_directory() {
    let (_dir, home) = recorded("opencode");
    let transcript = locate("opencode", &home);
    let first = transcript.read("").expect("the fixture reads");
    let db = home.join(".local/share/opencode/opencode.db");
    let connection = rusqlite::Connection::open(&db).unwrap();
    // A newer session for another directory must not be adopted.
    connection
        .execute(
            "INSERT INTO project VALUES ('proj_other', '/work/other', 'git', 'other', 1, ?1)",
            (9_999_999_999_999i64,),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session VALUES ('ses_other', 'proj_other', NULL, 'other', '/work/other', \
             'Other', '1.18.35', 1, ?1)",
            (9_999_999_999_999i64,),
        )
        .unwrap();
    // A third message on this session, with one part: the page gains exactly one.
    connection
        .execute(
            "INSERT INTO message VALUES ('msg_3', 'ses_fixture', 1768956700000, 1768956700000, ?1)",
            (r#"{"role":"user","time":{"created":1768956700000}}"#,),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO part VALUES ('prt_5', 'msg_3', 'ses_fixture', 1768956700000, \
             1768956700000, ?1)",
            (r#"{"type":"text","text":"Thanks."}"#,),
        )
        .unwrap();
    drop(connection);
    let page = transcript.read(&first.cursor).expect("the fixture reads");
    assert_eq!(page.messages.len(), 1, "{:?}", page.messages);
    assert_eq!(page.messages[0].text, "Thanks.");
    assert_eq!(page.cursor, "ses_fixture@3");
    assert_eq!(locate("opencode", &home).session(), "ses_fixture");
}

/// An installation that still has the pre-1.18 folder store is read with the
/// store format, and reads the same conversation the database holds: the two
/// shapes are one store, and the harness table is the only thing that says
/// which one a CLI wrote.
#[test]
fn the_older_folder_store_reads_the_same_conversation() {
    let (_dir, home) = recorded("opencode-store");
    let spec = shprd_agents::spec::TranscriptSpec {
        format: Some(TranscriptFormat::OpencodeStorage),
        location: Some("{data_home}/opencode/storage".to_string()),
    };
    let transcript =
        Transcript::locate(&spec, &home, Path::new(CWD)).expect("the older fixture store is found");
    assert_eq!(transcript.session(), "ses_fixture");
    assert_eq!(transcript.format(), TranscriptFormat::OpencodeStorage);
    let page = transcript.read("").expect("the fixture reads");
    assert_eq!(page.messages.len(), 2);
    assert_eq!(page.messages[0].text, "Review the last commit.");
    assert_eq!(page.messages[1].text, "Reading the commit now.");
    assert_eq!(page.messages[1].tools[0].name, "bash");
    assert_eq!(page.cursor, "ses_fixture@2");
}

#[cfg(unix)]
#[test]
fn a_record_is_matched_through_a_symlinked_directory() {
    let (dir, home) = recorded("opencode");
    // The session records `/work/repo`, which is not on this machine. Renew the
    // same session under a real directory, then ask for it by a symlink to that
    // directory: the reader follows the link rather than comparing the words.
    let real = dir.path().join("real");
    fs::create_dir_all(&real).unwrap();
    let link = dir.path().join("linked");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let db = home.join(".local/share/opencode/opencode.db");
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection
        .execute(
            "UPDATE session SET directory = ?1 WHERE id = 'ses_fixture'",
            (real.display().to_string(),),
        )
        .unwrap();
    drop(connection);
    let transcript = Transcript::locate(harness("opencode").transcript(), &home, &link)
        .expect("the session is found through the symlink");
    assert_eq!(transcript.session(), "ses_fixture");
}

#[cfg(unix)]
#[test]
fn a_claude_session_is_found_through_a_symlinked_directory() {
    // The same case the OpenCode store has a test for: a session the CLI wrote
    // under the resolved path belongs to the directory a person names by its
    // symlink, so the slug is built from the resolved path too.
    let (dir, home) = recorded("claude");
    let real = dir.path().join("real");
    fs::create_dir_all(&real).unwrap();
    let link = dir.path().join("linked");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let from = home.join(".claude").join("projects").join("-work-repo");
    let to = home
        .join(".claude")
        .join("projects")
        .join(cwd_slug(TranscriptFormat::ClaudeJsonl, &real));
    fs::rename(&from, &to).unwrap();
    let transcript = Transcript::locate(harness("claude").transcript(), &home, &link)
        .expect("the session is found through the symlink");
    assert_eq!(transcript.session(), "5f0c-session");
    let page = transcript.read("").expect("the fixture reads");
    assert_eq!(page.messages.len(), 4);
}

/// SHPR-65 bug 5: the chat of an agent that started at a given moment shows the
/// first session begun after it, never an earlier one in the same directory (the
/// person's own, or the last coordinator's).
#[test]
fn an_agent_reads_the_session_begun_after_it_started_not_an_older_one_in_the_folder() {
    let dir = tempfile::tempdir().expect("a temp home");
    let folder = dir
        .path()
        .join(".claude")
        .join("projects")
        .join("-work-repo");
    fs::create_dir_all(&folder).unwrap();
    let user = |text: &str| {
        format!(
            "{}\n",
            serde_json::json!({"type": "user", "message": {"role": "user", "content": text}})
        )
    };
    fs::write(folder.join("older.jsonl"), user("an earlier conversation")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1_600));
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let roots = shprd_agents::transcript::Roots::at(dir.path());
    let spec = harness("claude");
    // Nothing has been said since the agent started: no conversation is its.
    assert!(
        Transcript::locate_since(spec.transcript(), &roots, Path::new(CWD), Some(started))
            .is_none(),
        "the older session is not this agent's"
    );
    // The old rule still shows the newest one, whoever's it is.
    assert_eq!(
        Transcript::locate_in(spec.transcript(), &roots, Path::new(CWD))
            .unwrap()
            .session(),
        "older"
    );
    fs::write(folder.join("mine.jsonl"), user("hello")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    fs::write(folder.join("someone-elses-later.jsonl"), user("another")).unwrap();
    let found = Transcript::locate_since(spec.transcript(), &roots, Path::new(CWD), Some(started))
        .expect("the agent's session is found");
    assert_eq!(
        found.session(),
        "mine",
        "the first one begun after it started"
    );
}

/// OpenCode (both stores): an agent's session is one *created* after it started.
/// A session that began earlier and was merely touched since is somebody else's.
#[test]
fn opencode_pins_a_session_by_its_creation_not_its_last_update() {
    // The fixture session was created at 1768956000000 and updated at 1768956700000.
    for program in ["opencode", "opencode-store"] {
        let (_dir, home) = recorded(program);
        let roots = shprd_agents::transcript::Roots::at(&home);
        let spec = if program == "opencode" {
            harness("opencode").transcript().clone()
        } else {
            shprd_agents::spec::TranscriptSpec {
                format: Some(TranscriptFormat::OpencodeStorage),
                location: Some("{data_home}/opencode/storage".to_string()),
            }
        };
        let spec = &spec;
        let after_creation = Some(1_768_956_100_000);
        assert!(
            Transcript::locate_since(spec, &roots, Path::new(CWD), after_creation).is_none(),
            "{program}: begun before the agent started, only updated since"
        );
        let before_creation = Some(1_768_955_000_000);
        assert!(
            Transcript::locate_since(spec, &roots, Path::new(CWD), before_creation).is_some(),
            "{program}: begun after the agent started"
        );
    }
}

#[test]
fn a_program_with_no_transcript_section_has_nothing_to_read() {
    let kilo = harness("kilo");
    assert!(!kilo.transcript().readable());
    assert!(
        Transcript::locate(kilo.transcript(), Path::new("/home/you"), Path::new(CWD)).is_none()
    );
}

#[test]
fn a_session_that_was_never_written_is_no_transcript() {
    let empty = tempfile::tempdir().expect("a temp home");
    assert!(
        Transcript::locate(harness("claude").transcript(), empty.path(), Path::new(CWD)).is_none()
    );
}

#[test]
fn every_shipped_harness_that_names_a_format_names_its_location() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/harness");
    let specs = shprd_agents::spec::load_dir(&dir).expect("the harness table loads");
    let readable: Vec<&str> = specs
        .iter()
        .filter(|spec| spec.transcript.readable())
        .map(|spec| spec.id.as_str())
        .collect();
    assert_eq!(readable, ["claude", "codex", "opencode", "pi"]);
}

/// A folder store whose session files are not JSON at all is no transcript rather
/// than an error, so a half-written session file cannot break the panel.
#[test]
fn an_unreadable_store_file_is_skipped() {
    let (dir, home) = recorded("opencode-store");
    let spec = shprd_agents::spec::TranscriptSpec {
        format: Some(TranscriptFormat::OpencodeStorage),
        location: Some("{data_home}/opencode/storage".to_string()),
    };
    let session = home.join(".local/share/opencode/storage/session/proj_fixture/ses_fixture.json");
    fs::write(&session, "{not json").unwrap();
    assert!(Transcript::locate(&spec, &home, Path::new(CWD)).is_none());
    dir.close().unwrap();
}

/// A database whose session row names no directory at all is still adopted: an
/// older OpenCode wrote the folder only into the message parts.
#[test]
fn a_database_session_with_no_directory_is_still_adopted() {
    let (_dir, home) = recorded("opencode");
    let db = home.join(".local/share/opencode/opencode.db");
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection
        .execute(
            "UPDATE session SET directory = '' WHERE id = 'ses_fixture'",
            (),
        )
        .unwrap();
    drop(connection);
    // An empty directory is not `cwd`, so this is *not* adopted... unless the
    // reader treats "records nothing" as "belongs anywhere", which is the rule:
    // a session that says nothing is not refused.
    let transcript = locate("opencode", &home);
    assert_eq!(transcript.session(), "ses_fixture");
}

/// A session that has just started has no conversation yet, which is an empty
/// page rather than an error: the panel shows it as waiting.
#[test]
fn a_session_with_no_messages_yet_is_an_empty_page() {
    let (dir, home) = recorded("opencode");
    let transcript = locate("opencode", &home);
    let db = home.join(".local/share/opencode/opencode.db");
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection.execute("DELETE FROM part", ()).unwrap();
    connection.execute("DELETE FROM message", ()).unwrap();
    drop(connection);
    let page = transcript.read("").expect("an empty store reads");
    assert!(page.messages.is_empty());
    assert!(!page.more);
    dir.close().unwrap();
}

/// A database that is not a database at all is no transcript rather than a
/// panic, so a half-written file cannot break the panel.
#[test]
fn a_corrupt_database_is_no_transcript() {
    let (dir, home) = recorded("opencode");
    let db = home.join(".local/share/opencode/opencode.db");
    fs::write(&db, "not a database").unwrap();
    assert!(Transcript::locate(harness("opencode").transcript(), &home, Path::new(CWD)).is_none());
    dir.close().unwrap();
}

/// A folder store whose message folder is not a folder at all answers
/// [`TranscriptError::List`] rather than panicking.
#[test]
fn a_corrupt_store_is_an_error_not_a_panic() {
    let (dir, home) = recorded("opencode-store");
    let spec = shprd_agents::spec::TranscriptSpec {
        format: Some(TranscriptFormat::OpencodeStorage),
        location: Some("{data_home}/opencode/storage".to_string()),
    };
    let transcript =
        Transcript::locate(&spec, &home, Path::new(CWD)).expect("the older fixture store is found");
    let messages = home.join(".local/share/opencode/storage/message");
    fs::remove_dir_all(&messages).unwrap();
    fs::write(&messages, "not a folder").unwrap();
    match transcript.read("") {
        Err(TranscriptError::List { .. }) => {}
        other => panic!("expected a list error, got {other:?}"),
    }
    dir.close().unwrap();
}

// ---------------------------------------------------------------------------
// Finding a session at a realistic path
// ---------------------------------------------------------------------------
//
// The gate's bug was not the reading: it was the finding. A session sat in the
// folder the CLI really wrote while the reader looked in a folder it had spelled
// itself. These tests therefore write each program's session into the folder
// *the program's own rule* names, under a path shaped like the ones this machine
// really has (a temp dir under `/private/var/folders` with an underscore in it, a
// task worktree with dots and dashes), and read it back through the shipped
// harness line. If the rule and the CLI ever disagree again, one of these fails.

use shprd_agents::transcript::Roots;

/// A directory with every shape the gate's reports had trouble with: an
/// underscore (which Claude folds and pi does not), a dot, a space, and dashes.
///
/// It is created inside the test's temp directory, and the *resolved* path is
/// what a CLI would record, so these tests exercise the same
/// `/private/var/…`-versus-`/tmp/…` question a real session has on macOS.
fn tricky_dir(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// A claude session for `cwd` at the folder Claude Code's rule names.
fn write_claude(home: &Path, cwd: &Path, session: &str, words: &str) -> PathBuf {
    let folder = home
        .join(".claude")
        .join("projects")
        .join(cwd_slug(TranscriptFormat::ClaudeJsonl, cwd));
    fs::create_dir_all(&folder).unwrap();
    let path = folder.join(format!("{session}.jsonl"));
    let line = serde_json::json!({
        "type": "user",
        "isSidechain": false,
        "timestamp": "2026-11-04T10:00:00.000Z",
        "message": {"role": "user", "content": words},
    });
    fs::write(&path, format!("{line}\n")).unwrap();
    path
}

/// A pi session for `cwd` at the folder pi's rule names.
fn write_pi(home: &Path, cwd: &Path, session: &str, words: &str) -> PathBuf {
    let folder = home
        .join(".pi")
        .join("agent")
        .join("sessions")
        .join(cwd_slug(TranscriptFormat::PiJsonl, cwd));
    fs::create_dir_all(&folder).unwrap();
    let path = folder.join(format!("2026-11-04T00-00-00-000Z_{session}.jsonl"));
    let header = serde_json::json!({
        "type": "session",
        "version": 3,
        "id": session,
        "timestamp": "2026-11-04T00:00:00.000Z",
        "cwd": cwd.display().to_string(),
    });
    let line = serde_json::json!({
        "type": "message",
        "id": "u1",
        "timestamp": "2026-11-04T00:00:01.000Z",
        "message": {"role": "user", "timestamp": 1_794_616_780_000i64, "content": [{"type": "text", "text": words}]},
    });
    fs::write(&path, format!("{header}\n{line}\n")).unwrap();
    path
}

/// An OpenCode database with one session of two messages in it.
fn write_opencode(home: &Path, cwd: &Path, session: &str, said: [&str; 2]) -> PathBuf {
    let dir = home.join(".local").join("share").join("opencode");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("opencode.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT NOT NULL, vcs TEXT,
                name TEXT, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
             CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT NOT NULL,
                parent_id TEXT, slug TEXT NOT NULL, directory TEXT NOT NULL,
                title TEXT NOT NULL, version TEXT NOT NULL, time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL,
                session_id TEXT NOT NULL, time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL, data TEXT NOT NULL);",
        )
        .unwrap();
    let directory = cwd.display().to_string();
    connection
        .execute(
            "INSERT INTO project VALUES ('proj_fixture', ?1, 'git', 'repo', 1, 2)",
            (&directory,),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session VALUES (?1, 'proj_fixture', NULL, 'quiet-river', ?2, \
             'Week 46', '1.18.35', 1, 2)",
            (session, &directory),
        )
        .unwrap();
    for (index, role) in ["user", "assistant"].iter().enumerate() {
        let id = format!("msg_{index}");
        let data = format!(r#"{{"role":"{role}","time":{{"created":{}}}}},"#, index);
        let data = data.trim_end_matches(',').to_string();
        connection
            .execute(
                "INSERT INTO message VALUES (?1, ?2, ?3, ?3, ?4)",
                (&id, session, index as i64, &data),
            )
            .unwrap();
        let part = format!(
            r#"{{"type":"text","text":{}}}"#,
            serde_json::to_string(said[index]).unwrap()
        );
        connection
            .execute(
                "INSERT INTO part VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
                (format!("prt_{index}"), &id, session, index as i64, &part),
            )
            .unwrap();
    }
    drop(connection);
    path
}

/// Every program's session is found through the shipped table at a path with an
/// underscore, a dot, a space and a `/.tmpXXXX` segment in it — the shapes that
/// broke the gate.
#[test]
fn every_program_is_found_at_a_path_with_underscores_dots_and_spaces() {
    let homes = tempfile::tempdir().expect("a temp home");
    let workspaces = tempfile::tempdir().expect("a temp workspace");
    let home = homes.path().canonicalize().unwrap();
    for name in [
        "hp-501/210041/SHP-62/my_repo.v2 space",
        "hp-501/210041/SHP-62/.tmpAbC123",
    ] {
        let cwd = tricky_dir(workspaces.path(), name);
        let roots = Roots::at(&home);
        for (program, session) in [("claude", "c-1"), ("pi", "p-1")] {
            let _ = fs::remove_dir_all(home.join(".claude"));
            let _ = fs::remove_dir_all(home.join(".pi"));
            let path = match program {
                "claude" => write_claude(&home, &cwd, session, "Read the run log."),
                _ => write_pi(&home, &cwd, session, "Start task T3."),
            };
            let transcript = Transcript::locate_in(harness(program).transcript(), &roots, &cwd)
                .unwrap_or_else(|| {
                    panic!("{program} at {}: no transcript of {path:?}", cwd.display())
                });
            let page = transcript.read("").expect("the fixture reads");
            assert_eq!(page.messages.len(), 1, "{program}: {:?}", page.messages);
            assert!(page.messages[0].text.starts_with(if program == "claude" {
                "Read the run log."
            } else {
                "Start task T3."
            }));
        }
        // OpenCode is one database for the whole machine, keyed by directory.
        let _ = fs::remove_dir_all(home.join(".local"));
        write_opencode(&home, &cwd, "ses_tricky", ["Review it.", "Reading."]);
        let transcript = Transcript::locate_in(harness("opencode").transcript(), &roots, &cwd)
            .expect("opencode's database session is found");
        assert_eq!(transcript.session(), "ses_tricky");
        let page = transcript.read("").expect("the fixture reads");
        assert_eq!(page.messages.len(), 2, "{:?}", page.messages);
        assert_eq!(page.messages[0].text, "Review it.");
        assert_eq!(page.messages[1].text, "Reading.");
    }
}

/// A path with an underscore is the gate's own reproduction: Claude folds the
/// underscore, so the folder it writes is not the folder a `/`-and-`.`-only fold
/// names. The test states both spellings, which is what pins the rule.
#[test]
fn claude_reads_a_folder_whose_name_carries_a_folded_underscore() {
    let homes = tempfile::tempdir().expect("a temp home");
    let workspace = tempfile::tempdir().expect("a temp workspace");
    let home = homes.path().canonicalize().unwrap();
    let cwd = tricky_dir(workspace.path(), "hp_501/gn_T_hp/repo");
    let path = write_claude(&home, &cwd, "s1", "What did the last pass do?");
    // The folder really is the underscore-folded one, and the approximation the
    // gate found is a folder that does not exist.
    let folder = path.parent().unwrap();
    assert!(
        folder
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("-gn-T-hp-"),
        "{}",
        folder.display()
    );
    let wrong = cwd.to_string_lossy().replace(['/', '.'], "-");
    assert_ne!(folder.file_name().unwrap().to_string_lossy(), wrong);
    assert!(!home.join(".claude/projects").join(wrong).exists());
    let transcript =
        Transcript::locate_in(harness("claude").transcript(), &Roots::at(&home), &cwd).unwrap();
    assert_eq!(transcript.session(), "s1");
    let page = transcript.read("").unwrap();
    assert_eq!(page.messages[0].text, "What did the last pass do?");
}

/// A pi path keeps its underscore, which is the other half of the same rule: the
/// two programs disagree, and the reader has to know which one it is asking.
#[test]
fn pi_reads_a_folder_whose_name_keeps_its_underscore() {
    let homes = tempfile::tempdir().expect("a temp home");
    let workspace = tempfile::tempdir().expect("a temp workspace");
    let home = homes.path().canonicalize().unwrap();
    let cwd = tricky_dir(workspace.path(), "hp_501/gn_T_hp/repo.v2");
    let path = write_pi(&home, &cwd, "p1", "Start task T3.");
    let folder = path.parent().unwrap();
    let name = folder.file_name().unwrap().to_string_lossy().to_string();
    assert!(name.starts_with("--") && name.ends_with("--"), "{name}");
    assert!(name.contains("hp_501"), "pi keeps the underscore: {name}");
    assert!(name.contains("repo.v2"), "pi keeps the dot: {name}");
    let transcript =
        Transcript::locate_in(harness("pi").transcript(), &Roots::at(&home), &cwd).unwrap();
    assert_eq!(
        transcript.read("").unwrap().messages[0].text,
        "Start task T3."
    );
}

/// The reader looks under the roots it is given and nowhere else: a session that
/// is in one home is not found when the chat is pointed at another, which is the
/// rule that keeps `SHPRD_HOME` out of a transcript lookup.
#[test]
fn a_session_is_found_only_under_the_roots_it_was_given() {
    let sessions = tempfile::tempdir().expect("a temp home");
    let other = tempfile::tempdir().expect("another home");
    let workspace = tempfile::tempdir().expect("a temp workspace");
    let cwd = tricky_dir(workspace.path(), "repo");
    let real = sessions.path().canonicalize().unwrap();
    write_claude(&real, &cwd, "s1", "Read the run log.");
    assert!(
        Transcript::locate_in(harness("claude").transcript(), &Roots::at(&real), &cwd).is_some()
    );
    let wrong = Roots::at(other.path());
    assert!(
        Transcript::locate_in(harness("claude").transcript(), &wrong, &cwd).is_none(),
        "a session in one home must not be found under another (SHPRD_HOME is not the CLI's home)"
    );
}

/// `{data_home}` is a root of its own: with `$XDG_DATA_HOME` set, OpenCode's
/// database is there and not under `~/.local/share`.
#[test]
fn the_data_home_root_follows_the_xdg_setting() {
    let home_dir = tempfile::tempdir().expect("a temp home");
    let xdg = tempfile::tempdir().expect("an xdg data home");
    let workspace = tempfile::tempdir().expect("a temp workspace");
    let home = home_dir.path().canonicalize().unwrap();
    let data_home = xdg.path().canonicalize().unwrap();
    let cwd = tricky_dir(workspace.path(), "repo");
    // The database is written under the XDG home this time.
    let dir = data_home.join("opencode");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("opencode.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL,
                time_updated INTEGER NOT NULL);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, data TEXT NOT NULL);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL,
                session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL);",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session VALUES ('ses_xdg', ?1, 5)",
            (cwd.display().to_string(),),
        )
        .unwrap();
    drop(connection);
    let roots = Roots::new(&home, Some(&data_home));
    let transcript = Transcript::locate_in(harness("opencode").transcript(), &roots, &cwd)
        .expect("the database under $XDG_DATA_HOME is found");
    assert_eq!(transcript.session(), "ses_xdg");
    // With the default rule it is not found, which is what `{data_home}` means.
    assert!(
        Transcript::locate_in(harness("opencode").transcript(), &Roots::at(&home), &cwd).is_none()
    );
}

/// The roots a launch will read are the ones its own environment carries.
#[test]
fn the_roots_of_a_launch_come_from_its_environment() {
    let mut base = std::collections::BTreeMap::new();
    base.insert("HOME".to_string(), "/home/dev".to_string());
    let roots = Roots::for_launch(&base);
    assert_eq!(roots.home, Path::new("/home/dev"));
    assert_eq!(
        roots.data_home,
        Path::new("/home/dev/.local/share"),
        "an unset XDG home is the default the CLI resolves"
    );
    base.insert("XDG_DATA_HOME".to_string(), "/xdg/data".to_string());
    let roots = Roots::for_launch(&base);
    assert_eq!(roots.data_home, Path::new("/xdg/data"));
    // An empty variable is unset, as the XDG rule says.
    base.insert("XDG_DATA_HOME".to_string(), String::new());
    base.insert("HOME".to_string(), String::new());
    let roots = Roots::for_launch(&base);
    assert_eq!(roots.home, PathBuf::new());
    assert_eq!(roots.data_home, PathBuf::new());
}

/// Rewrite the claude fixture's record as `count` numbered user lines, with a
/// padded body so the file is large; returns the transcript and its size.
fn big_record(home: &Path, count: usize) -> (Transcript, u64) {
    let transcript = locate("claude", home);
    let path = transcript.path().unwrap().to_path_buf();
    let pad = "x".repeat(180);
    let mut body = String::new();
    for n in 0..count {
        // Every tenth line is bookkeeping, which is not a message.
        if n % 10 == 9 {
            body.push_str("{\"type\":\"queue-operation\"}\n");
        }
        body.push_str(&format!(
            "{{\"type\":\"user\",\"isSidechain\":false,\"timestamp\":\"2026-11-04T10:00:00.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"m{n} {pad}\"}}}}\n"
        ));
    }
    fs::write(&path, &body).unwrap();
    (transcript, body.len() as u64)
}

fn number_of(text: &str) -> usize {
    text.trim_start_matches('m')
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn the_tail_of_a_large_record_is_the_newest_page_and_opens_fast() {
    let (_dir, home) = recorded("claude");
    // 80,000 lines of ~280 bytes is a record of about 22 MB.
    let (transcript, size) = big_record(&home, 80_000);
    assert!(size > 20 * 1024 * 1024, "{size}");
    let started = std::time::Instant::now();
    let tail = transcript.read_tail().expect("the tail reads");
    assert!(
        started.elapsed() < std::time::Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(tail.messages.len(), shprd_agents::transcript::MAX_MESSAGES);
    assert_eq!(number_of(&tail.messages.last().unwrap().text), 79_999);
    assert!(tail.older.is_some());
    // Nothing is newer than the tail: a forward read from its cursor is empty.
    let after = transcript.read(&tail.cursor).expect("the fixture reads");
    assert!(after.messages.is_empty());
}

#[test]
fn backward_pages_join_the_tail_with_nothing_lost_or_repeated() {
    let (_dir, home) = recorded("claude");
    let (transcript, _) = big_record(&home, 1_234);
    let mut numbers = Vec::new();
    let tail = transcript.read_tail().unwrap();
    let mut pages = vec![tail.messages.clone()];
    let mut older = tail.older.clone();
    while let Some(cursor) = older {
        let page = transcript.read_before(&cursor).unwrap();
        older = page.older.clone();
        pages.push(page.messages);
    }
    // Oldest page first.
    for page in pages.iter().rev() {
        for message in page {
            numbers.push(number_of(&message.text));
        }
    }
    assert_eq!(numbers, (0..1_234).collect::<Vec<_>>());
    // A tail then a forward read of what is appended afterwards.
    append_line(
        transcript.path().unwrap(),
        r#"{"type":"user","isSidechain":false,"timestamp":"2026-11-04T10:01:00.000Z","message":{"role":"user","content":"m1234 new"}}"#,
    );
    let page = transcript.read(&tail.cursor).unwrap();
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].text, "m1234 new");
}

#[test]
fn a_small_record_is_one_tail_page_with_nothing_older() {
    let (_dir, home) = recorded("claude");
    let transcript = locate("claude", &home);
    let tail = transcript.read_tail().unwrap();
    assert_eq!(tail.messages.len(), 4);
    assert!(tail.older.is_none());
    let whole = transcript.read("").unwrap();
    assert_eq!(tail.messages, whole.messages);
    assert_eq!(tail.cursor, whole.cursor);
}
