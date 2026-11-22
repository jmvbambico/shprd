//! The forge seam: everything shprd knows about pull requests.
//!
//! Work lands through pull requests, and shprd drives them through a forge —
//! `gh` in production, [`FakeForge`] in tests. Since M8.3 every task has its
//! own pull request, so the caller passes the task's branch to [`Forge::find_open`]
//! and the commit it means to merge to [`Forge::merge`]; the run-wide path is
//! kept for the old integration branch and uses the same calls. The trait is the
//! whole contract: find an open pull request, create one, refresh its body, read
//! its status, merge one commit. Nothing here pushes, and nothing here decides
//! policy: the caller asks the questions, and [`PrStatus`] holds the answers.
//!
//! Every command runs as a program, never through a shell, so a branch name or
//! a title can never turn into a command.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use shprd_core::spawn;
use thiserror::Error;

/// A pull request, as the forge numbers and links it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pr {
    /// The forge's number for it.
    pub number: u64,
    /// The URL a human opens to read, approve or merge it.
    pub url: String,
    /// The branch it proposes to merge.
    pub head: String,
}

/// What the forge says about a pull request right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrStatus {
    /// True when a human approved the pull request.
    pub approved: bool,
    /// Whether its checks passed: `None` while they are still running.
    pub checks_passed: Option<bool>,
    /// True when the forge has merged it.
    pub merged: bool,
    /// The commit the pull request's head branch points at.
    pub head_sha: String,
}

/// Why a forge operation failed.
#[derive(Debug, Error)]
pub enum ForgeError {
    /// The forge's program could not be started at all.
    #[error("could not run the forge: {source}")]
    Spawn {
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The forge's program ran and exited non-zero.
    #[error("forge {} failed (exit {code:?}): {stderr}", args.join(" "))]
    Failed {
        /// The arguments, without the leading program name.
        args: Vec<String>,
        /// The exit code, when the process reported one.
        code: Option<i32>,
        /// Whatever it wrote to stderr, trimmed.
        stderr: String,
    },
    /// A script or an answer could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// The file that failed.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The forge's output was well-formed enough to start but not to use.
    #[error("cannot read the forge's answer ({what}): {detail}")]
    Parse {
        /// What was being parsed.
        what: String,
        /// What was wrong with it.
        detail: String,
    },
}

/// The forge operations a landing needs.
pub trait Forge {
    /// The open pull request whose head is `head`, if there is one.
    ///
    /// # Errors
    ///
    /// Fails when the forge cannot be asked.
    fn find_open(&self, head: &str) -> Result<Option<Pr>, ForgeError>;

    /// Opens a pull request from `head` into `base`, with `title` and the body
    /// read from `body_file`.
    ///
    /// # Errors
    ///
    /// Fails when the forge refuses to create it.
    fn create(
        &self,
        base: &str,
        head: &str,
        title: &str,
        body_file: &Path,
    ) -> Result<Pr, ForgeError>;

    /// Replaces the body of pull request `number` with `body_file`'s contents.
    ///
    /// # Errors
    ///
    /// Fails when the forge refuses to edit it.
    fn edit_body(&self, number: u64, body_file: &Path) -> Result<(), ForgeError>;

    /// What the forge says about pull request `number`.
    ///
    /// # Errors
    ///
    /// Fails when the forge cannot be asked.
    fn status(&self, number: u64) -> Result<PrStatus, ForgeError>;

    /// Squash-merges pull request `number`, and only while its head is
    /// `head_sha`.
    ///
    /// Naming the commit is what keeps a landing honest: a branch that moved
    /// between the read and the merge is refused by the forge rather than
    /// merged at a commit nobody looked at.
    ///
    /// # Errors
    ///
    /// Fails when the forge refuses to merge it.
    fn merge(&self, number: u64, head_sha: &str) -> Result<(), ForgeError>;

    /// Whether pull request `number` merges cleanly into its base right now:
    /// `Some(false)` when the forge says it conflicts, `None` when it cannot
    /// say yet (a forge computes this lazily) or does not report it.
    ///
    /// # Errors
    ///
    /// Fails when the forge cannot be asked.
    fn mergeable(&self, number: u64) -> Result<Option<bool>, ForgeError> {
        let _ = number;
        Ok(None)
    }

    /// Closes pull request `number` unmerged, leaving `comment` on it.
    ///
    /// A forge that cannot close one does nothing: the stale pull request is
    /// then left for a human, and a fresh attempt opens its own.
    ///
    /// # Errors
    ///
    /// Fails when the forge refuses to close it.
    fn close(&self, number: u64, comment: &str) -> Result<(), ForgeError> {
        let _ = (number, comment);
        Ok(())
    }
}

/// The real forge: `gh`, run in the repository the pull request belongs to.
///
/// The directory decides which repository `gh` talks about, so it is always an
/// explicit path and never the caller's working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhForge {
    /// The directory `gh` runs in.
    dir: PathBuf,
    /// The program to run: `gh`, or a test double naming itself `gh`.
    program: String,
}

impl GhForge {
    /// A forge that runs `gh` in `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            program: String::from("gh"),
        }
    }

    /// The same forge, running `program` instead of `gh`.
    ///
    /// Only tests use this: it lets a script stand in for `gh` so the argument
    /// vectors and the parsing are exercised without a network or a login.
    #[cfg(test)]
    fn with_program(dir: impl Into<PathBuf>, program: &str) -> Self {
        Self {
            dir: dir.into(),
            program: program.to_string(),
        }
    }

    /// Runs the program and returns its raw output, whatever its exit status.
    fn exec(&self, args: &[String]) -> Result<Output, ForgeError> {
        let mut command = Command::new(&self.program);
        command
            .args(args)
            .current_dir(&self.dir)
            .env("GH_PROMPT_DISABLED", "1")
            .env("LC_ALL", "C")
            .stdin(Stdio::null());
        spawn::output(&mut command).map_err(|source| ForgeError::Spawn { source })
    }

    /// Runs the program and returns its trimmed stdout, or why it failed.
    fn run(&self, args: &[String]) -> Result<String, ForgeError> {
        let output = self.exec(args)?;
        if !output.status.success() {
            return Err(ForgeError::Failed {
                args: args.to_vec(),
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// `gh pr view <n> --json reviewDecision,state,headRefOid`, parsed.
    fn view(&self, number: u64) -> Result<(bool, bool, String), ForgeError> {
        let args = vec![
            "pr".to_string(),
            "view".to_string(),
            number.to_string(),
            "--json".to_string(),
            "reviewDecision,state,headRefOid".to_string(),
        ];
        let text = self.run(&args)?;
        let value: Value = self.parse(&text, "pr view")?;
        let approved = matches!(value.get("reviewDecision"), Some(Value::String(decision)) if decision == "APPROVED");
        let merged = matches!(value.get("state"), Some(Value::String(state)) if state == "MERGED");
        let head_sha = self.text(&value, "headRefOid")?;
        Ok((approved, merged, head_sha))
    }

    /// `gh pr checks <n>`, read as a verdict: 0 passed, 8 still running, any
    /// other failure.
    fn checks(&self, number: u64) -> Result<Option<bool>, ForgeError> {
        let args = vec!["pr".to_string(), "checks".to_string(), number.to_string()];
        let output = self.exec(&args)?;
        match output.status.code() {
            Some(0) => Ok(Some(true)),
            // 8 is `gh`'s "checks still pending": not a failure, no verdict.
            Some(8) => Ok(None),
            _ => Ok(Some(false)),
        }
    }

    /// Parses one JSON answer, naming what it was for.
    fn parse(&self, text: &str, what: &str) -> Result<Value, ForgeError> {
        serde_json::from_str(text).map_err(|err| ForgeError::Parse {
            what: what.to_string(),
            detail: format!("{err}: {text}"),
        })
    }

    /// One string field of a JSON object.
    fn text(&self, value: &Value, key: &str) -> Result<String, ForgeError> {
        match value.get(key).and_then(Value::as_str) {
            Some(text) => Ok(text.to_string()),
            None => Err(ForgeError::Parse {
                what: format!("field {key}"),
                detail: value.to_string(),
            }),
        }
    }

    /// The number in a pull request URL, which is the number `gh` created.
    fn number_from_url(url: &str) -> Result<u64, ForgeError> {
        url.rsplit('/')
            .next()
            .and_then(|tail| tail.parse::<u64>().ok())
            .ok_or_else(|| ForgeError::Parse {
                what: String::from("pull request number in the url"),
                detail: url.to_string(),
            })
    }
}

impl Forge for GhForge {
    fn find_open(&self, head: &str) -> Result<Option<Pr>, ForgeError> {
        let args = vec![
            "pr".to_string(),
            "list".to_string(),
            "--head".to_string(),
            head.to_string(),
            "--state".to_string(),
            "open".to_string(),
            "--json".to_string(),
            "number,url".to_string(),
        ];
        let text = self.run(&args)?;
        let value = self.parse(&text, "pr list")?;
        let found = match value.as_array().and_then(|rows| rows.first()) {
            Some(row) => row.clone(),
            None => return Ok(None),
        };
        let number = found.get("number").and_then(Value::as_u64);
        let number = number.ok_or_else(|| ForgeError::Parse {
            what: String::from("field number"),
            detail: found.to_string(),
        })?;
        Ok(Some(Pr {
            number,
            url: self.text(&found, "url")?,
            head: head.to_string(),
        }))
    }

    fn create(
        &self,
        base: &str,
        head: &str,
        title: &str,
        body_file: &Path,
    ) -> Result<Pr, ForgeError> {
        let args = vec![
            "pr".to_string(),
            "create".to_string(),
            "--base".to_string(),
            base.to_string(),
            "--head".to_string(),
            head.to_string(),
            "--title".to_string(),
            title.to_string(),
            "--body-file".to_string(),
            body_file.display().to_string(),
        ];
        let url = self.run(&args)?;
        Ok(Pr {
            number: Self::number_from_url(&url)?,
            url,
            head: head.to_string(),
        })
    }

    fn edit_body(&self, number: u64, body_file: &Path) -> Result<(), ForgeError> {
        let args = vec![
            "pr".to_string(),
            "edit".to_string(),
            number.to_string(),
            "--body-file".to_string(),
            body_file.display().to_string(),
        ];
        self.run(&args).map(|_| ())
    }

    fn status(&self, number: u64) -> Result<PrStatus, ForgeError> {
        let (approved, merged, head_sha) = self.view(number)?;
        Ok(PrStatus {
            approved,
            checks_passed: self.checks(number)?,
            merged,
            head_sha,
        })
    }

    fn merge(&self, number: u64, head_sha: &str) -> Result<(), ForgeError> {
        let args = vec![
            "pr".to_string(),
            "merge".to_string(),
            number.to_string(),
            "--squash".to_string(),
            "--match-head-commit".to_string(),
            head_sha.to_string(),
        ];
        self.run(&args).map(|_| ())
    }

    fn mergeable(&self, number: u64) -> Result<Option<bool>, ForgeError> {
        let args = vec![
            "pr".to_string(),
            "view".to_string(),
            number.to_string(),
            "--json".to_string(),
            "mergeable".to_string(),
        ];
        let text = self.run(&args)?;
        let value: Value = self.parse(&text, "pr view mergeable")?;
        Ok(match value.get("mergeable").and_then(Value::as_str) {
            Some("CONFLICTING") => Some(false),
            Some("MERGEABLE") => Some(true),
            _ => None,
        })
    }

    fn close(&self, number: u64, comment: &str) -> Result<(), ForgeError> {
        let args = vec![
            "pr".to_string(),
            "close".to_string(),
            number.to_string(),
            "--comment".to_string(),
            comment.to_string(),
        ];
        self.run(&args).map(|_| ())
    }
}

/// A forge that records what it was asked and answers from a script.
///
/// Tests use it in two shapes. In-process, [`FakeForge::new`] starts empty and
/// [`FakeForge::calls`] is the record. Across processes — the CLI integration
/// tests drive the real binary — [`FakeForge::from_script`] reads its answers
/// from a JSON file and appends every call to `<script>.calls`, so the test can
/// script a status before each invocation and read what was asked afterwards.
#[derive(Debug)]
pub struct FakeForge {
    /// What the fake knows and what it has been asked.
    state: RefCell<FakeState>,
    /// Where calls are appended, when the fake is shared with another process.
    journal: Option<PathBuf>,
}

/// The whole state of a [`FakeForge`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct FakeState {
    /// The pull requests `find_open` may answer with.
    open: Vec<Pr>,
    /// The status `status` answers with, per number.
    statuses: BTreeMap<u64, PrStatus>,
    /// Every call made, in order.
    calls: Vec<String>,
    /// The number the next `create` uses.
    next_number: u64,
    /// The URL the next `create` answers with, when the script sets one.
    create_url: Option<String>,
    /// True when `merge` must fail, standing in for a forge that refuses.
    merge_fails: bool,
    /// True when `merge` leaves the pull request unmerged, standing in for a
    /// forge that accepted the call without merging anything.
    merge_silent: bool,
    /// The pull requests the forge reports as conflicting with their base.
    conflicting: BTreeSet<u64>,
}

impl Default for FakeState {
    fn default() -> Self {
        Self {
            open: Vec::new(),
            statuses: BTreeMap::new(),
            calls: Vec::new(),
            next_number: 1,
            create_url: None,
            merge_fails: false,
            merge_silent: false,
            conflicting: BTreeSet::new(),
        }
    }
}

impl FakeForge {
    /// A scripted forge with no answers: every status is refused-looking.
    pub fn new() -> Self {
        Self {
            state: RefCell::new(FakeState::default()),
            journal: None,
        }
    }

    /// A forge scripted by the JSON file at `path`, journalling beside it.
    ///
    /// The script holds `open`, `status`, `next_number` and `create_url`; every
    /// call is appended to `<path>.calls`, one line each.
    pub fn from_script(path: &Path) -> Result<Self, ForgeError> {
        let text = std::fs::read_to_string(path).map_err(|source| ForgeError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let value: Value = serde_json::from_str(&text).map_err(|err| ForgeError::Parse {
            what: format!("script {}", path.display()),
            detail: err.to_string(),
        })?;
        let forge = Self {
            state: RefCell::new(Self::state_from(&value)?),
            journal: Some(PathBuf::from(format!("{}.calls", path.display()))),
        };
        Ok(forge)
    }

    /// Builds the state a script holds.
    fn state_from(value: &Value) -> Result<FakeState, ForgeError> {
        let mut state = FakeState::default();
        if let Some(rows) = value.get("open").and_then(Value::as_array) {
            for row in rows {
                state.open.push(Self::pr_from(row)?);
            }
        }
        if let Some(rows) = value.get("status").and_then(Value::as_array) {
            for row in rows {
                let number = row.get("number").and_then(Value::as_u64);
                let number = number.ok_or_else(|| ForgeError::Parse {
                    what: String::from("script row number"),
                    detail: row.to_string(),
                })?;
                if row.get("mergeable").and_then(Value::as_bool) == Some(false) {
                    state.conflicting.insert(number);
                }
                state.statuses.insert(number, Self::status_from(row)?);
            }
        }
        if let Some(next) = value.get("next_number").and_then(Value::as_u64) {
            state.next_number = next;
        }
        if let Some(url) = value.get("create_url").and_then(Value::as_str) {
            state.create_url = Some(url.to_string());
        }
        if let Some(fails) = value.get("merge_fails").and_then(Value::as_bool) {
            state.merge_fails = fails;
        }
        Ok(state)
    }

    /// One pull request out of a script row.
    fn pr_from(row: &Value) -> Result<Pr, ForgeError> {
        let number = row.get("number").and_then(Value::as_u64);
        let number = number.ok_or_else(|| ForgeError::Parse {
            what: String::from("script row number"),
            detail: row.to_string(),
        })?;
        Ok(Pr {
            number,
            url: Self::text_from(row, "url")?,
            head: Self::text_from(row, "head")?,
        })
    }

    /// One status out of a script row; a missing `checks_passed` means running.
    fn status_from(row: &Value) -> Result<PrStatus, ForgeError> {
        Ok(PrStatus {
            approved: row
                .get("approved")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            checks_passed: row.get("checks_passed").and_then(Value::as_bool),
            merged: row.get("merged").and_then(Value::as_bool).unwrap_or(false),
            head_sha: Self::text_from(row, "head_sha")?,
        })
    }

    /// One string field of a script row.
    fn text_from(row: &Value, key: &str) -> Result<String, ForgeError> {
        match row.get(key).and_then(Value::as_str) {
            Some(text) => Ok(text.to_string()),
            None => Err(ForgeError::Parse {
                what: format!("script field {key}"),
                detail: row.to_string(),
            }),
        }
    }

    /// The calls made so far, in order.
    pub fn calls(&self) -> Vec<String> {
        self.state.borrow().calls.clone()
    }

    /// Makes every `merge` fail, as a forge that refused would.
    pub fn refuse_merges(&self) {
        self.state.borrow_mut().merge_fails = true;
    }

    /// Makes every `merge` succeed without merging, as a forge that accepted
    /// the call and did nothing would. It is how a caller's check that the
    /// merge settled gets tested.
    pub fn silence_merges(&self) {
        self.state.borrow_mut().merge_silent = true;
    }

    /// Makes `mergeable` report pull request `number` as conflicting.
    pub fn set_conflicting(&self, number: u64) {
        self.state.borrow_mut().conflicting.insert(number);
    }

    /// Scripts the status `status` for pull request `number`.
    pub fn set_status(&self, number: u64, status: PrStatus) {
        self.state.borrow_mut().statuses.insert(number, status);
    }

    /// Scripts `pr` as the open pull request `find_open` answers with.
    pub fn add_open(&self, pr: Pr) {
        self.state.borrow_mut().open.push(pr);
    }

    /// Records one call, in memory and in the journal when there is one.
    fn record(&self, line: String) {
        self.state.borrow_mut().calls.push(line.clone());
        if let Some(journal) = &self.journal {
            let mut text = self.state.borrow().calls.join("\n");
            text.push('\n');
            let _ = std::fs::write(journal, text);
        }
    }
}

impl Default for FakeForge {
    fn default() -> Self {
        Self::new()
    }
}

impl Forge for FakeForge {
    fn find_open(&self, head: &str) -> Result<Option<Pr>, ForgeError> {
        self.record(format!("find_open {head}"));
        let found = self
            .state
            .borrow()
            .open
            .iter()
            .find(|pr| pr.head == head)
            .cloned();
        Ok(found)
    }

    fn create(
        &self,
        base: &str,
        head: &str,
        title: &str,
        body_file: &Path,
    ) -> Result<Pr, ForgeError> {
        self.record(format!(
            "create {base} {head} {title} {}",
            body_file.display()
        ));
        let mut state = self.state.borrow_mut();
        let number = state.next_number;
        state.next_number += 1;
        let url = match &state.create_url {
            Some(url) => url.clone(),
            None => format!("https://example.invalid/pull/{number}"),
        };
        let number = url
            .rsplit('/')
            .next()
            .and_then(|tail| tail.parse::<u64>().ok())
            .unwrap_or(number);
        Ok(Pr {
            number,
            url,
            head: head.to_string(),
        })
    }

    fn edit_body(&self, number: u64, body_file: &Path) -> Result<(), ForgeError> {
        self.record(format!("edit_body {number} {}", body_file.display()));
        Ok(())
    }

    fn status(&self, number: u64) -> Result<PrStatus, ForgeError> {
        self.record(format!("status {number}"));
        Ok(self
            .state
            .borrow()
            .statuses
            .get(&number)
            .cloned()
            .unwrap_or(PrStatus {
                approved: false,
                checks_passed: None,
                merged: false,
                head_sha: String::new(),
            }))
    }

    fn mergeable(&self, number: u64) -> Result<Option<bool>, ForgeError> {
        self.record(format!("mergeable {number}"));
        Ok(self
            .state
            .borrow()
            .conflicting
            .contains(&number)
            .then_some(false))
    }

    fn close(&self, number: u64, comment: &str) -> Result<(), ForgeError> {
        self.record(format!("close {number} {comment}"));
        let mut state = self.state.borrow_mut();
        state.open.retain(|pr| pr.number != number);
        Ok(())
    }

    fn merge(&self, number: u64, head_sha: &str) -> Result<(), ForgeError> {
        self.record(format!("merge {number} {head_sha}"));
        let mut state = self.state.borrow_mut();
        if state.merge_fails {
            return Err(ForgeError::Failed {
                args: vec![
                    "pr".to_string(),
                    "merge".to_string(),
                    number.to_string(),
                    head_sha.to_string(),
                ],
                code: Some(1),
                stderr: "the forge refused the merge".to_string(),
            });
        }
        // A real merge moves the pull request to merged; the status keeps the
        // head it was reported at, which is the commit that landed.
        if !state.merge_silent
            && let Some(status) = state.statuses.get_mut(&number)
        {
            status.merged = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory holding a fake `gh` and the files that script its answers.
    struct Script {
        /// Keeps the directory alive.
        _temp: tempfile::TempDir,
        /// Where the script and its answers live.
        dir: PathBuf,
    }

    impl Script {
        /// Writes a fake `gh`, with the given `pr checks` exit code.
        fn new(checks_exit: &str) -> Script {
            let temp = match tempfile::tempdir() {
                Ok(temp) => temp,
                Err(err) => panic!("tempdir: {err}"),
            };
            let dir = temp.path().to_path_buf();
            let forge = Script { _temp: temp, dir };
            forge.write(
                "list.json",
                "[{\"number\":7,\"url\":\"https://example.invalid/pull/7\"}]",
            );
            forge.write(
                "view.json",
                "{\"reviewDecision\":\"APPROVED\",\"state\":\"OPEN\",\"headRefOid\":\"abc1234\"}",
            );
            forge.write("checks_exit", checks_exit);
            let body = "\
                #!/bin/sh\n\
                dir=$(dirname \"$0\")\n\
                echo \"$*\" >> \"$dir/calls\"\n\
                case \"$2\" in\n\
                  list) cat \"$dir/list.json\" ;;\n\
                  create) echo 'https://example.invalid/pull/8' ;;\n\
                  view) cat \"$dir/view.json\" ;;\n\
                  checks) exit \"$(cat \"$dir/checks_exit\")\" ;;\n\
                  edit|merge) exit 0 ;;\n\
                  *) echo 'unknown verb' >&2; exit 3 ;;\n\
                esac\n";
            forge.write_script("gh", body);
            forge
        }

        /// Writes one answer file.
        fn write(&self, name: &str, body: &str) {
            let path = self.dir.join(name);
            if let Err(err) = std::fs::write(&path, body) {
                panic!("write {}: {err}", path.display());
            }
        }

        /// Writes an executable stand-in through a temporary name beside it and
        /// a rename, so the path a test runs is never open for writing.
        fn write_script(&self, name: &str, body: &str) {
            let path = self.dir.join(name);
            let temp = self.dir.join(format!("{name}.tmp"));
            if let Err(err) = std::fs::write(&temp, body) {
                panic!("write {}: {err}", temp.display());
            }
            let mode = std::os::unix::fs::PermissionsExt::from_mode(0o755);
            if let Err(err) = std::fs::set_permissions(&temp, mode) {
                panic!("chmod {}: {err}", temp.display());
            }
            if let Err(err) = std::fs::rename(&temp, &path) {
                panic!("rename {} into place: {err}", path.display());
            }
        }

        /// A forge that runs this script.
        fn forge(&self) -> Result<GhForge, ForgeError> {
            let path = self.dir.join("gh");
            let program = match path.to_str() {
                Some(text) => text,
                None => panic!("the script path is not text"),
            };
            Ok(GhForge::with_program(&self.dir, program))
        }

        /// Everything the fake `gh` was called with, one call per line.
        fn calls(&self) -> String {
            match std::fs::read_to_string(self.dir.join("calls")) {
                Ok(text) => text,
                Err(err) => panic!("read the call log: {err}"),
            }
        }
    }

    fn status_of(script: &Script) -> PrStatus {
        match script.forge().and_then(|forge| forge.status(7)) {
            Ok(status) => status,
            Err(err) => panic!("status: {err}"),
        }
    }

    #[test]
    fn find_open_reads_the_first_open_pull_request() {
        let script = Script::new("0");
        let found = match script
            .forge()
            .and_then(|forge| forge.find_open("shprd/r1/int"))
        {
            Ok(found) => found,
            Err(err) => panic!("find_open: {err}"),
        };
        assert_eq!(
            found,
            Some(Pr {
                number: 7,
                url: "https://example.invalid/pull/7".to_string(),
                head: "shprd/r1/int".to_string(),
            })
        );
        assert!(
            script
                .calls()
                .contains("pr list --head shprd/r1/int --state open --json number,url"),
            "{}",
            script.calls()
        );
    }

    #[test]
    fn find_open_with_no_rows_is_none() {
        let script = Script::new("0");
        script.write("list.json", "[]");
        let found = match script
            .forge()
            .and_then(|forge| forge.find_open("shprd/r1/int"))
        {
            Ok(found) => found,
            Err(err) => panic!("find_open: {err}"),
        };
        assert_eq!(found, None);
    }

    #[test]
    fn create_reads_the_number_out_of_the_url() {
        let script = Script::new("0");
        let body = script.dir.join("body.md");
        let created = script
            .forge()
            .and_then(|forge| forge.create("main", "shprd/r1/int", "run t1", body.as_path()));
        let created = match created {
            Ok(created) => created,
            Err(err) => panic!("create: {err}"),
        };
        assert_eq!(created.number, 8);
        assert_eq!(created.url, "https://example.invalid/pull/8");
        let calls = script.calls();
        assert!(
            calls.contains("pr create --base main --head shprd/r1/int"),
            "{calls}"
        );
        assert!(calls.contains("--title run t1 --body-file"), "{calls}");
    }

    #[test]
    fn status_reports_approval_head_and_passing_checks() {
        let script = Script::new("0");
        assert_eq!(
            status_of(&script),
            PrStatus {
                approved: true,
                checks_passed: Some(true),
                merged: false,
                head_sha: "abc1234".to_string(),
            }
        );
    }

    #[test]
    fn pending_checks_are_no_verdict_and_failed_checks_are_a_failure() {
        let pending = Script::new("8");
        assert_eq!(status_of(&pending).checks_passed, None, "exit 8 is pending");
        let failed = Script::new("1");
        assert_eq!(
            status_of(&failed).checks_passed,
            Some(false),
            "any other exit is a failure"
        );
    }

    #[test]
    fn a_merged_and_unapproved_pull_request_reads_as_such() {
        let script = Script::new("0");
        script.write(
            "view.json",
            "{\"reviewDecision\":\"REVIEW_REQUIRED\",\"state\":\"MERGED\",\"headRefOid\":\"fff9999\"}",
        );
        let status = status_of(&script);
        assert!(!status.approved);
        assert!(status.merged);
        assert_eq!(status.head_sha, "fff9999");
    }

    #[test]
    fn edit_and_merge_name_the_pull_request() {
        let script = Script::new("0");
        let body = script.dir.join("body.md");
        let outcome = script.forge().and_then(|forge| {
            forge.edit_body(7, body.as_path())?;
            forge.merge(7, "abc1234")
        });
        assert!(outcome.is_ok(), "edit then merge must both work");
        let calls = script.calls();
        assert!(calls.contains("pr edit 7 --body-file"), "{calls}");
        assert!(
            calls.contains("pr merge 7 --squash --match-head-commit abc1234"),
            "{calls}"
        );
    }

    #[test]
    fn a_failing_forge_reports_its_exit_code_and_stderr() {
        let script = Script::new("0");
        script.write_script("gh", "#!/bin/sh\necho 'boom' >&2\nexit 3\n");
        let err = match script.forge().and_then(|forge| forge.find_open("a")) {
            Ok(found) => panic!("a failing gh must not answer: {found:?}"),
            Err(err) => err,
        };
        assert!(
            matches!(&err, ForgeError::Failed { code: Some(3), .. }),
            "{err}"
        );
        assert!(err.to_string().contains("boom"), "{err}");
    }

    /// A status a test scripts for the fake.
    fn passed(head: &str) -> PrStatus {
        PrStatus {
            approved: true,
            checks_passed: Some(true),
            merged: false,
            head_sha: head.to_string(),
        }
    }

    #[test]
    fn the_fake_records_every_call_and_answers_from_its_script() {
        let forge = FakeForge::new();
        forge.set_status(7, passed("abc1234"));
        let body = Path::new("/tmp/body.md");
        forge.add_open(Pr {
            number: 7,
            url: "https://example.invalid/pull/7".to_string(),
            head: "shprd/r1/int".to_string(),
        });
        let outcome = (|| -> Result<(), ForgeError> {
            let found = forge.find_open("shprd/r1/int")?;
            assert_eq!(found.map(|pr| pr.number), Some(7));
            forge.edit_body(7, body)?;
            let created = forge.create("main", "shprd/r1/int", "run t1", body)?;
            assert_eq!(created.number, 1, "the fake numbers from one");
            assert_eq!(forge.status(7)?, passed("abc1234"));
            forge.merge(7, "abc1234")?;
            Ok(())
        })();
        assert!(outcome.is_ok());
        assert_eq!(
            forge.calls(),
            vec![
                "find_open shprd/r1/int".to_string(),
                "edit_body 7 /tmp/body.md".to_string(),
                "create main shprd/r1/int run t1 /tmp/body.md".to_string(),
                "status 7".to_string(),
                "merge 7 abc1234".to_string(),
            ]
        );
    }

    #[test]
    fn a_scripted_fake_reads_its_answers_and_writes_its_journal() {
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("tempdir: {err}"),
        };
        let path = dir.path().join("forge.json");
        let script = serde_json::json!({
            "open": [{"number": 7, "url": "https://example.invalid/pull/7", "head": "shprd/r1/int"}],
            "status": [{
                "number": 7,
                "approved": false,
                "checks_passed": true,
                "merged": false,
                "head_sha": "abc1234",
            }],
        });
        if let Err(err) = std::fs::write(&path, script.to_string()) {
            panic!("write the script: {err}");
        }
        let forge = match FakeForge::from_script(&path) {
            Ok(forge) => forge,
            Err(err) => panic!("from_script: {err}"),
        };
        let found = match forge.find_open("shprd/r1/int") {
            Ok(found) => found,
            Err(err) => panic!("find_open: {err}"),
        };
        assert_eq!(found.map(|pr| pr.number), Some(7));
        let status = match forge.status(7) {
            Ok(status) => status,
            Err(err) => panic!("status: {err}"),
        };
        assert!(!status.approved);
        assert_eq!(status.checks_passed, Some(true));
        let journal = PathBuf::from(format!("{}.calls", path.display()));
        let text = std::fs::read_to_string(&journal).unwrap_or_default();
        assert!(text.contains("find_open shprd/r1/int"), "{text}");
        assert!(text.contains("status 7"), "{text}");
    }
}
