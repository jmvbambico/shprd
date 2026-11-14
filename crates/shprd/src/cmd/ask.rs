//! `shprd ask hook`: what a program's ask hook runs.
//!
//! A Claude Code or Codex `PermissionRequest` hook hands the program's payload
//! on stdin; this verb turns it into an [`Ask`](shprd_core::ask::Ask) in the
//! run's queue, records it in the ledger, and waits for the person's answer.
//! The answer is printed in the shape the program's hook expects, and only then
//! does the agent carry on.
//!
//! It never decides by itself: a timeout, a payload it cannot read, a task the
//! run does not know or an error writing the queue all print nothing, so the
//! program's own prompt stays and the person can still answer there.
//!
//! The flow follows Shepherdr's ask hook (MIT, `mods/chat/ask-hook.sh`,
//! `service/asks.ts`, tag `v3.1.34`, commit `a3f327a`): the hook waits, the
//! person answers, and no hook invents a decision.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use serde_json::{Map, Value, json};
use shprd_core::ask::{Ask, AskKind, AskQueue};
use shprd_core::event::{Actor, EventKind};
use shprd_core::log::{Clock, SystemClock};

use crate::cli::{AskHookArgs, AskKindArg};
use crate::ctx::{CliError, Ctx, resolve_root};

/// The environment variable a worker's launch sets with its task id.
const TASK_ENV: &str = "SHPRD_TASK";

/// How long a hook waits when nothing says otherwise, seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 1800;

/// How often a waiting hook looks for an answer.
const POLL: Duration = Duration::from_millis(100);

/// A counter that keeps two asks made in the same millisecond apart.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// The parts of a program's hook payload shprd reads.
#[derive(Debug, Default)]
struct Payload {
    /// The tool the agent called, e.g. `Bash` or `AskUserQuestion`.
    tool: String,
    /// The tool's input, kept whole so a question's answer can be put back.
    input: Value,
}

impl Payload {
    /// Reads a hook payload, or nothing when stdin is not one.
    fn read() -> Self {
        let mut raw = String::new();
        if std::io::stdin().read_to_string(&mut raw).is_err() {
            return Self::default();
        }
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            return Self::default();
        };
        Self {
            tool: value
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            input: value.get("tool_input").cloned().unwrap_or(Value::Null),
        }
    }
}

/// One ask as the hook made it, with what the program needs to print its
/// answer afterwards.
#[derive(Debug)]
struct Words {
    kind: AskKind,
    text: String,
    options: Vec<String>,
    /// The first question's own words, which Claude's `updatedInput` keys its
    /// answers by.
    question: Option<String>,
}

/// Turns a hook payload into the ask a card shows.
fn words(payload: &Payload) -> Option<Words> {
    let input = payload.input.as_object();
    match payload.tool.as_str() {
        "AskUserQuestion" => {
            let questions = input
                .and_then(|input| input.get("questions"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let first = questions.first()?;
            let text = first
                .get("question")
                .and_then(Value::as_str)
                .unwrap_or("A question")
                .to_string();
            let options = first
                .get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .filter_map(|option| {
                            option
                                .get("label")
                                .and_then(Value::as_str)
                                .or_else(|| option.as_str())
                                .map(strip_recommended)
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(Words {
                kind: AskKind::Question,
                question: Some(text.clone()),
                text,
                options,
            })
        }
        "ExitPlanMode" => {
            let plan = input
                .and_then(|input| input.get("plan"))
                .and_then(Value::as_str)
                .unwrap_or("A plan to approve")
                .to_string();
            Some(Words {
                kind: AskKind::Plan,
                text: plan,
                options: vec!["Approve".to_string(), "Deny".to_string()],
                question: None,
            })
        }
        _ => {
            let text = input
                .and_then(|input| {
                    input
                        .get("description")
                        .and_then(Value::as_str)
                        .or_else(|| input.get("command").and_then(Value::as_str))
                        .or_else(|| input.get("file_path").and_then(Value::as_str))
                })
                .map(str::to_string)
                .or_else(|| (!payload.tool.is_empty()).then(|| format!("Use {}", payload.tool)))?;
            Some(Words {
                kind: AskKind::Permission,
                text,
                options: vec!["Allow".to_string(), "Deny".to_string()],
                question: None,
            })
        }
    }
}

/// A choice label without the "(Recommended)" tag Claude puts on one.
fn strip_recommended(label: &str) -> String {
    let trimmed = label.trim_end();
    let without = trimmed
        .strip_suffix("(Recommended)")
        .or_else(|| trimmed.strip_suffix("(recommended)"))
        .unwrap_or(trimmed);
    without.trim_end().to_string()
}

/// Runs `ask hook`.
pub fn hook(args: &AskHookArgs, repo: Option<&Path>) -> Result<(), CliError> {
    let Some(task) = args.task.clone().or_else(|| {
        std::env::var(TASK_ENV)
            .ok()
            .filter(|value| !value.is_empty())
    }) else {
        // Without a task there is nowhere to put the card; the program's own
        // prompt is the only way left to answer.
        return Ok(());
    };
    let root = resolve_run_repo(repo)?;
    let payload = Payload::read();
    let made = match (&args.kind, &args.text) {
        // A test, or a program whose payload shprd cannot read, may spell the
        // ask out directly.
        (Some(kind), text) => {
            let kind = match kind {
                AskKindArg::Question => AskKind::Question,
                AskKindArg::Permission => AskKind::Permission,
                AskKindArg::Plan => AskKind::Plan,
            };
            let text = text
                .clone()
                .unwrap_or_else(|| default_text(kind).to_string());
            Some(Words {
                kind,
                question: (kind == AskKind::Question).then(|| text.clone()),
                text,
                options: args.options.clone(),
            })
        }
        (None, _) => words(&payload),
    };
    let Some(made) = made else {
        return Ok(());
    };
    let now_ms = SystemClock.now_ms();
    let timeout_secs = args.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS);
    let id = args.id.clone().unwrap_or_else(|| ask_id(now_ms));
    let ask = Ask::new(
        id.clone(),
        task.clone(),
        made.kind,
        made.text.clone(),
        now_ms,
    )
    .with_options(made.options.clone())
    .expires_at(now_ms.saturating_add(timeout_secs.saturating_mul(1_000)));
    let actor = Actor::Agent {
        id: args.program.clone(),
    };
    let ctx = Ctx::new(root, actor, false, true);
    let raised = EventKind::AskRaised {
        task: task.clone(),
        ask: id.clone(),
        ask_kind: made.kind,
        text: made.text.clone(),
        options: made.options.clone(),
        created_at_ms: now_ms,
        expires_at_ms: ask.expires_at_ms,
    };
    // Refused or unreadable: the hook says nothing and lets the program ask.
    if ctx.check(&raised).is_err() {
        return Ok(());
    }
    let queue = AskQueue::for_repo(&ctx.root);
    if queue.raise(&ask).is_err() {
        return Ok(());
    }
    if ctx.commit(raised).is_err() {
        let _ = queue.remove(&task, &id);
        return Ok(());
    }
    wait_and_print(&queue, &task, &id, made, &payload, timeout_secs)
}

/// What a flag-only ask says when it names no text.
fn default_text(kind: AskKind) -> &'static str {
    match kind {
        AskKind::Question => "A question",
        AskKind::Permission => "Permission to run a tool",
        AskKind::Plan => "A plan to approve",
    }
}

/// Answers the waiting hook prints, in the shape Claude Code and Codex read.
///
/// Both programs take the same `hookSpecificOutput` envelope for a
/// `PermissionRequest`; a question's answer also goes back as the tool's input
/// with `answers`, and a plan or permission is an `allow` or a `deny`.
fn decision(kind: AskKind, question: Option<&str>, input: &Value, answer: &str) -> Value {
    let denied = is_denial(answer);
    if denied {
        let message = match kind {
            AskKind::Plan => "The person did not approve the plan.".to_string(),
            AskKind::Question => "The person dismissed the question without answering.".to_string(),
            AskKind::Permission => "The person declined this in shprd.".to_string(),
        };
        return json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": { "behavior": "deny", "message": message },
            }
        });
    }
    if kind != AskKind::Question {
        return json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": { "behavior": "allow" },
            }
        });
    }
    let mut updated = input.as_object().cloned().unwrap_or_else(Map::new);
    let mut answers = Map::new();
    answers.insert(
        question.unwrap_or("Question").to_string(),
        Value::String(answer.trim().to_string()),
    );
    updated.insert("answers".to_string(), Value::Object(answers));
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": { "behavior": "allow", "updatedInput": Value::Object(updated) },
        }
    })
}

/// Whether an answer means "no".
fn is_denial(answer: &str) -> bool {
    let word = answer
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(|c: char| !c.is_ascii_alphabetic())
        .to_ascii_lowercase();
    matches!(
        word.as_str(),
        "deny" | "denied" | "reject" | "rejected" | "decline" | "declined" | "no"
    )
}

/// Waits for the answer, prints the decision, and returns.
fn wait_and_print(
    queue: &AskQueue,
    task: &str,
    id: &str,
    made: Words,
    payload: &Payload,
    timeout_secs: u64,
) -> Result<(), CliError> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match queue.answer_of(task, id) {
            Ok(Some(answer)) => {
                let value = decision(
                    made.kind,
                    made.question.as_deref(),
                    &payload.input,
                    &answer.text,
                );
                let text = serde_json::to_string(&value).map_err(|err| {
                    CliError::other(anyhow!(err).context("serializing the decision"))
                })?;
                println!("{text}");
                return Ok(());
            }
            Ok(None) => {}
            // A broken answer file is not a decision; keep waiting until the
            // deadline rather than answering on the program's behalf.
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            return Ok(());
        }
        std::thread::sleep(POLL);
    }
}

/// An ask id: the time, the process and a counter, so two hooks in one
/// millisecond still differ.
fn ask_id(now_ms: u64) -> String {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("{now_ms}.{}.{n}", std::process::id())
}

/// The run's repository, from a worktree when the hook runs in one.
///
/// A worker's working directory is its worktree, outside the repository, so
/// `git rev-parse --show-toplevel` names the worktree and not the run. The git
/// common directory's parent is the repository whose `.shprd/` holds the log
/// and the ask queue.
fn resolve_run_repo(explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    let root = resolve_root(explicit)?;
    if root.join(".git").is_dir() || root.join(".shprd").is_dir() {
        return Ok(root);
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["rev-parse", "--git-common-dir"])
        .output();
    let Ok(output) = output else {
        return Ok(root);
    };
    if !output.status.success() {
        return Ok(root);
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return Ok(root);
    }
    let common = PathBuf::from(text);
    let common = if common.is_absolute() {
        common
    } else {
        root.join(common)
    };
    // A linked worktree's common dir is `<repo>/.git`; its parent is the repo.
    if common.file_name().and_then(|name| name.to_str()) == Some(".git")
        && let Some(parent) = common.parent()
    {
        return Ok(parent.to_path_buf());
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::{Payload, decision, is_denial, strip_recommended, words};
    use shprd_core::ask::AskKind;

    #[test]
    fn a_permission_payload_names_what_the_agent_runs() {
        let payload = Payload {
            tool: "Bash".to_string(),
            input: json!({"command": "cargo test", "description": "Run the tests"}),
        };
        let made = words(&payload).expect("words");
        assert_eq!(made.kind, AskKind::Permission);
        assert_eq!(made.text, "Run the tests");
        assert_eq!(made.options, vec!["Allow", "Deny"]);
    }

    #[test]
    fn a_question_payload_keeps_its_first_question_and_choices() {
        let payload = Payload {
            tool: "AskUserQuestion".to_string(),
            input: json!({"questions": [{
                "question": "Which database?",
                "options": [
                    {"label": "SQLite"},
                    {"label": "Postgres (Recommended)"}
                ]
            }]}),
        };
        let made = words(&payload).expect("words");
        assert_eq!(made.kind, AskKind::Question);
        assert_eq!(made.text, "Which database?");
        assert_eq!(made.question.as_deref(), Some("Which database?"));
        assert_eq!(made.options, vec!["SQLite", "Postgres"]);
    }

    #[test]
    fn a_plan_payload_is_a_plan_to_approve() {
        let payload = Payload {
            tool: "ExitPlanMode".to_string(),
            input: json!({"plan": "1. Add a queue\n2. Add a hook"}),
        };
        let made = words(&payload).expect("words");
        assert_eq!(made.kind, AskKind::Plan);
        assert!(made.text.contains("Add a queue"));
    }

    #[test]
    fn an_answer_is_denied_only_when_it_says_no() {
        for answer in ["deny", "Deny", "rejected", "no", "No."] {
            assert!(is_denial(answer), "{answer}");
        }
        for answer in ["allow", "approve", "yes", "use SQLite"] {
            assert!(!is_denial(answer), "{answer}");
        }
    }

    #[test]
    fn a_permission_allow_is_the_empty_decision_the_hook_expects() {
        let value = decision(AskKind::Permission, None, &json!({}), "allow");
        assert_eq!(
            value["hookSpecificOutput"]["decision"],
            json!({"behavior": "allow"})
        );
        assert_eq!(
            value["hookSpecificOutput"]["hookEventName"],
            "PermissionRequest"
        );
    }

    #[test]
    fn a_question_answer_goes_back_as_the_tools_answers() {
        let value = decision(
            AskKind::Question,
            Some("Which database?"),
            &json!({"questions": []}),
            "SQLite",
        );
        assert_eq!(
            value["hookSpecificOutput"]["decision"]["updatedInput"]["answers"]["Which database?"],
            "SQLite"
        );
    }

    #[test]
    fn a_denied_plan_says_why() {
        let value = decision(AskKind::Plan, None, &json!({}), "deny");
        assert_eq!(value["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert!(
            value["hookSpecificOutput"]["decision"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("did not approve"),
            "{value}"
        );
    }

    #[test]
    fn the_recommended_tag_is_taken_off_a_choice() {
        assert_eq!(strip_recommended("Postgres (Recommended)"), "Postgres");
        assert_eq!(strip_recommended("SQLite"), "SQLite");
    }
}
