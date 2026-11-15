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
use shprd_agents::transcript::{Role, Transcript, TranscriptError};

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
    (dir, home)
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
    assert_eq!(page.messages.len(), 3);
    assert_eq!(page.messages[0].role, Role::User);
    assert_eq!(
        page.messages[0].text,
        "Read the run log and tell me what the last pass did."
    );
    assert!(page.messages[0].tools.is_empty());
    assert_eq!(page.messages[1].role, Role::Assistant);
    assert_eq!(page.messages[1].text, "I will read the tail of the log.");
    assert_eq!(page.messages[1].tools[0].name, "Bash");
    assert_eq!(page.messages[1].tools[0].summary, "shprd tick --once");
    assert_eq!(page.messages[2].role, Role::Assistant);
    assert_eq!(
        page.messages[2].text,
        "The last pass started T2 and landed T1."
    );
    assert!(page.messages[2].at_ms.is_some());
    // The subagent's sidechain and the tool result are in the file and are not
    // messages: a person did not read them and the agent did not say them.
    let text = format!("{:?}", page.messages);
    assert!(!text.contains("A subagent's own prompt."));
    assert!(!text.contains("tool_result"));
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
    assert_eq!(page.messages.len(), 2, "the half-written line is not read");
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
        3,
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
    assert_eq!(page.messages.len(), 3, "{:?}", page.messages);
    assert_eq!(page.messages[0].role, Role::User);
    assert_eq!(page.messages[0].text, "Review the diff on branch work/12.");
    assert_eq!(page.messages[1].role, Role::Assistant);
    assert_eq!(page.messages[1].text, "I will read the diff first.");
    assert_eq!(page.messages[2].tools.len(), 1);
    assert_eq!(page.messages[2].tools[0].name, "exec");
    assert!(
        !format!("{:?}", page.messages).contains("You are Codex."),
        "a developer message is the program's own brief, not the conversation"
    );
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
fn opencode_joins_a_session_of_message_files_and_its_parts() {
    let (_dir, home) = recorded("opencode");
    let transcript = locate("opencode", &home);
    assert_eq!(transcript.session(), "ses_fixture");
    let page = transcript.read("").expect("the fixture reads");
    assert_eq!(page.messages.len(), 2);
    assert_eq!(page.messages[0].role, Role::User);
    assert_eq!(page.messages[0].text, "Review the last commit.");
    assert_eq!(page.messages[1].role, Role::Assistant);
    assert_eq!(page.messages[1].text, "Reading the commit now.");
    assert_eq!(page.messages[1].tools.len(), 1);
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
    let store = home.join(".local/share/opencode/storage");
    // A newer session for another project must not be adopted.
    fs::create_dir_all(store.join("session/other")).unwrap();
    fs::write(
        store.join("session/other/ses_other.json"),
        r#"{"id":"ses_other","directory":"/work/other","time":{"created":1,"updated":9999999999999}}"#,
    )
    .unwrap();
    let message = store.join("message/ses_fixture/msg_3.json");
    fs::write(
        &message,
        r#"{"id":"msg_3","sessionID":"ses_fixture","role":"user","time":{"created":1768956700000}}"#,
    )
    .unwrap();
    fs::create_dir_all(store.join("part/msg_3")).unwrap();
    fs::write(
        store.join("part/msg_3/prt_5.json"),
        r#"{"id":"prt_5","sessionID":"ses_fixture","messageID":"msg_3","type":"text","text":"Thanks."}"#,
    )
    .unwrap();
    let page = transcript.read(&first.cursor).expect("the fixture reads");
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].text, "Thanks.");
    assert_eq!(page.cursor, "ses_fixture@3");
    assert_eq!(locate("opencode", &home).session(), "ses_fixture");
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
    let session = home.join(".local/share/opencode/storage/session/proj_fixture/ses_fixture.json");
    let text = fs::read_to_string(&session).unwrap();
    fs::write(
        &session,
        text.replace("/work/repo", &real.display().to_string()),
    )
    .unwrap();
    let transcript = Transcript::locate(harness("opencode").transcript(), &home, &link)
        .expect("the session is found through the symlink");
    assert_eq!(transcript.session(), "ses_fixture");
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

/// A store whose session files are not JSON at all is no transcript rather than
/// an error, so a half-written session file cannot break the panel.
#[test]
fn an_unreadable_store_file_is_skipped() {
    let (dir, home) = recorded("opencode");
    let session = home.join(".local/share/opencode/storage/session/proj_fixture/ses_fixture.json");
    fs::write(&session, "{not json").unwrap();
    assert!(Transcript::locate(harness("opencode").transcript(), &home, Path::new(CWD)).is_none());
    dir.close().unwrap();
}

/// A session that has just started has no conversation yet, which is an empty
/// page rather than an error: the panel shows it as waiting.
#[test]
fn a_session_with_no_messages_yet_is_an_empty_page() {
    let (dir, home) = recorded("opencode");
    let transcript = locate("opencode", &home);
    fs::remove_dir_all(home.join(".local/share/opencode/storage/message")).unwrap();
    let page = transcript.read("").expect("an empty store reads");
    assert!(page.messages.is_empty());
    assert!(!page.more);
    dir.close().unwrap();
}

/// A store whose message folder is not a folder at all answers
/// [`TranscriptError::List`] rather than panicking.
#[test]
fn a_corrupt_store_is_an_error_not_a_panic() {
    let (dir, home) = recorded("opencode");
    let transcript = locate("opencode", &home);
    let messages = home.join(".local/share/opencode/storage/message");
    fs::remove_dir_all(&messages).unwrap();
    fs::write(&messages, "not a folder").unwrap();
    match transcript.read("") {
        Err(TranscriptError::List { .. }) => {}
        other => panic!("expected a list error, got {other:?}"),
    }
    dir.close().unwrap();
}
