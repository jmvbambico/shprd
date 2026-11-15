//! `shprd ask hook` and `shprd ask raise`: how a card reaches the person.
//!
//! A Claude Code or Codex `PermissionRequest` hook hands the program's payload
//! on stdin to [`hook`]; this verb turns it into an
//! [`Ask`](shprd_core::ask::Ask) in the run's queue, records it in the ledger,
//! and waits for the person's answer. The answer is printed in the shape the
//! program's hook expects, and only then does the agent carry on.
//!
//! A program whose CLI has no ask hook cannot reach a card at all, so its
//! worker raises one itself with [`raise`]: the same queue file, the same
//! ledger event and the same wait, and the answer is printed in the person's own
//! words. The worker's brief picks between the two from the program's harness
//! (`[asks] supported`), never from a list of program names.
//!
//! Neither verb decides by itself: a timeout, a payload it cannot read or a
//! task the run does not know leaves the program's own prompt in place, so the
//! person can still answer there.
//!
//! The flow follows Shepherdr's ask hook (MIT, `mods/chat/ask-hook.sh`,
//! `service/asks.ts`, tag `v3.1.34`, commit `a3f327a`): the hook waits, the
//! person answers, and no hook invents a decision.

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use serde_json::{Map, Value, json};
use shprd_agents::asks::{AskHookInstall, AskHookInstaller, AskHookPaths};
use shprd_agents::data::DataDir;
use shprd_agents::harness::Harness;
use shprd_core::ask::{Ask, AskAnswer, AskError, AskKind, AskQueue};
use shprd_core::event::{Actor, EventKind};
use shprd_core::log::{Clock, SystemClock};
use shprd_core::rules::RuleError;

use crate::cli::{AskHookArgs, AskInstallArgs, AskKindArg, AskRaiseArgs};
use crate::ctx::{CliError, Ctx, resolve_checkout, resolve_root};

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
    let Some(task) = args.task.clone().or_else(task_of_env) else {
        // Without a task there is nowhere to put the card; the program's own
        // prompt is the only way left to answer.
        return Ok(());
    };
    // The hook runs in a worker's worktree, and the queue it writes to is the
    // run's: `resolve_root` follows a linked worktree to the repository.
    let root = resolve_root(repo)?;
    let payload = Payload::read();
    let made = match (&args.kind, &args.text) {
        // A test, or a program whose payload shprd cannot read, may spell the
        // ask out directly.
        (Some(kind), text) => {
            let kind = ask_kind(*kind);
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
    let timeout_secs = args
        .timeout_secs
        .or_else(|| harness_timeout(&args.program))
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    let id = args
        .id
        .clone()
        .unwrap_or_else(|| ask_id(SystemClock.now_ms()));
    let actor = Actor::Agent {
        id: args.program.clone(),
    };
    // Refused or unreadable: the hook says nothing and lets the program ask.
    if raise_card(&root, actor, &task, &made, &id, timeout_secs).is_err() {
        return Ok(());
    }
    let queue = AskQueue::for_repo(&root);
    let Some(answer) = wait_for_answer(&queue, &task, &id, timeout_secs) else {
        return Ok(());
    };
    let value = decision(
        made.kind,
        made.question.as_deref(),
        &payload.input,
        &answer.text,
    );
    let text = serde_json::to_string(&value)
        .map_err(|err| CliError::other(anyhow!(err).context("serializing the decision")))?;
    println!("{text}");
    Ok(())
}

/// Runs `ask raise`: the agent's own way to raise a needs-you card.
///
/// A program whose CLI has no ask hook cannot turn its own question into a
/// card, so its worker's brief tells it to run this instead. The card is raised
/// exactly as a hook raises it (same queue file, same ledger event, same
/// blocking wait); only the printed answer differs: this verb prints the
/// person's answer in their own words, because no program's hook shape is
/// waiting for it.
pub fn raise(args: &AskRaiseArgs, ctx: &Ctx) -> Result<(), CliError> {
    let task = match args.task.clone().or_else(task_of_env) {
        Some(task) => task,
        None => {
            // A coordinator has no task (`$SHPRD_TASK` is set only for a task),
            // so this points it at the run-wide verb rather than leaving it
            // with a refusal it cannot act on.
            return Err(CliError::other(anyhow!(
                "no task for this ask: run it in the task's worktree (where `$SHPRD_TASK` is set) \
                 or name one with `--task`; a run-wide question has no task, so raise it with \
                 `shprd agent escalate --body <question> --default <what happens if nobody answers>`"
            )));
        }
    };
    let text = args.text.trim().to_string();
    if text.is_empty() {
        return Err(CliError::other(anyhow!(
            "`--text` is empty: say what the person is asked, in one sentence"
        )));
    }
    let kind = ask_kind(args.kind);
    let options = match (kind, args.options.is_empty()) {
        (AskKind::Question, _) => args.options.clone(),
        // A permission or a plan is a yes or a no, and a card with no choices
        // would make the person type one.
        (_, true) => vec!["Allow".to_string(), "Deny".to_string()],
        (_, false) => args.options.clone(),
    };
    let made = Words {
        kind,
        question: (kind == AskKind::Question).then(|| text.clone()),
        text,
        options,
    };
    let timeout_secs = args.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS);
    let id = args
        .id
        .clone()
        .unwrap_or_else(|| ask_id(SystemClock.now_ms()));
    raise_card(
        &ctx.root,
        ctx.actor.clone(),
        &task,
        &made,
        &id,
        timeout_secs,
    )?;
    let queue = AskQueue::for_repo(&ctx.root);
    let Some(answer) = wait_for_answer(&queue, &task, &id, timeout_secs) else {
        return Err(CliError::other(anyhow!(
            "no answer within {timeout_secs}s: card {id:?} is still open, so raise it again if the \
             decision still blocks you, or hand in as blocked with the question in your summary"
        )));
    };
    if ctx.json {
        let ask = queue
            .read(&task, &id)
            .map_err(|err| CliError::other(anyhow!(err).context("reading the answered ask")))?;
        let text = serde_json::to_string_pretty(&ask)
            .map_err(|err| CliError::other(anyhow!(err).context("serializing the answered ask")))?;
        println!("{text}");
    } else {
        println!("{}", answer.text);
    }
    Ok(())
}

/// The task a worker's verbs read from the environment, when one is set.
fn task_of_env() -> Option<String> {
    std::env::var(TASK_ENV)
        .ok()
        .filter(|value| !value.is_empty())
}

/// One kind arg as the ask's kind.
fn ask_kind(kind: AskKindArg) -> AskKind {
    match kind {
        AskKindArg::Question => AskKind::Question,
        AskKindArg::Permission => AskKind::Permission,
        AskKindArg::Plan => AskKind::Plan,
    }
}

/// Raises one card in the run's queue and ledger, or says why it could not.
///
/// Every caller that raises a card goes through here, so the hook and
/// `ask raise` cannot raise two different things. The raise is create-new: only
/// the process whose write lands first owns the ask. A process that loses the
/// race must not touch the queue file — deleting it would leave the ledger's
/// `ask_raised` with no file to answer and strand every waiting caller until
/// their timeouts — so it waits like the owner does and says nothing about it.
fn raise_card(
    root: &Path,
    actor: Actor,
    task: &str,
    made: &Words,
    id: &str,
    timeout_secs: u64,
) -> Result<(), CliError> {
    let now_ms = SystemClock.now_ms();
    let ask = Ask::new(
        id.to_string(),
        task.to_string(),
        made.kind,
        made.text.clone(),
        now_ms,
    )
    .with_options(made.options.clone())
    .expires_at(now_ms.saturating_add(timeout_secs.saturating_mul(1_000)));
    let ctx = Ctx::new(root.to_path_buf(), actor, false, true);
    let raised = EventKind::AskRaised {
        task: task.to_string(),
        ask: id.to_string(),
        ask_kind: made.kind,
        text: made.text.clone(),
        options: made.options.clone(),
        created_at_ms: now_ms,
        expires_at_ms: ask.expires_at_ms,
    };
    ctx.check(&raised)?;
    let queue = AskQueue::for_repo(&ctx.root);
    let raised_here = match queue.raise(&ask) {
        Ok(()) => true,
        Err(AskError::AlreadyRaised { .. }) => false,
        Err(err) => {
            return Err(CliError::other(anyhow!(err).context("writing the ask")));
        }
    };
    match ctx.commit(raised) {
        Ok(_) => Ok(()),
        // The log already holds this ask: another caller won the race, or a
        // crashed one left its file behind. Keep the queue file so the ask
        // stays answerable, and wait for the answer like its owner would.
        Err(CliError::Refused(RuleError::AskAlreadyRaised { .. })) => Ok(()),
        Err(err) => {
            // We made the file and the log refused the ask, so this process's
            // write is the one to undo; a file it did not make is left alone.
            if raised_here {
                let _ = queue.remove(task, id);
            }
            Err(err)
        }
    }
}

/// Waits for the answer to `task`'s ask `id`, or `None` when the wait runs out.
///
/// A broken answer file is not a decision, so it keeps the caller waiting
/// rather than answering on the program's behalf.
fn wait_for_answer(queue: &AskQueue, task: &str, id: &str, timeout_secs: u64) -> Option<AskAnswer> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        // Still open, or a broken answer file: neither is a decision, so keep
        // waiting until the deadline.
        if let Ok(Some(answer)) = queue.answer_of(task, id) {
            return Some(answer);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(POLL);
    }
}

/// The ask timeout the program's own harness names, in seconds.
///
/// A run with no data directory, or a program shprd has no harness for, says
/// nothing, and the caller falls back to [`DEFAULT_TIMEOUT_SECS`].
fn harness_timeout(program: &str) -> Option<u64> {
    let data = crate::cmd::roster::resolve_data_dir();
    let harness = data.harness(program).ok()?;
    harness
        .asks()
        .timeout_seconds
        .map(u64::from)
        .filter(|secs| *secs > 0)
}

/// The program's harness, loaded and checked, naming where it was looked for.
fn load_harness(data: &DataDir, program: &str) -> Result<Harness, CliError> {
    let rel = format!("harness/{program}.toml");
    data.harness(program).map_err(|err| {
        CliError::other(anyhow!(err).context(format!(
            "no harness for {program:?} at {} (set SHPRD_DATA_DIR to a directory holding `harness/`)",
            data.path(&rel).display()
        )))
    })
}

/// Runs `ask install`: writes the program's ask hook into the task's worktree.
///
/// The config file is the program's own worktree-local one
/// (`.claude/settings.local.json`, `.codex/hooks.json`), never the person's
/// global config; the installer refuses a harness that names a `~/...` target.
pub fn install(args: &AskInstallArgs, repo: Option<&Path>) -> Result<(), CliError> {
    let data = crate::cmd::roster::resolve_data_dir();
    let harness = load_harness(&data, &args.program)?;
    // The installer writes into the checkout it is pointed at, so a worktree
    // stays a worktree here: only the run's log follows the repository.
    let worktree = match &args.worktree {
        Some(path) => path.clone(),
        None => resolve_checkout(repo)?,
    };
    let binary = std::env::current_exe()
        .map_err(|err| CliError::other(anyhow!(err).context("finding the shprd binary")))?;
    let hook = args
        .hook
        .clone()
        .unwrap_or_else(|| worktree.join(".shprd").join("ask-hook.sh"));
    let installer = harness.ask_installer();
    let paths = AskHookPaths::new(&hook, &worktree, &binary);
    let outcome = installer.install(&paths).map_err(|err| {
        CliError::other(anyhow!(err).context(format!(
            "installing the {} ask hook in {}",
            args.program,
            worktree.display()
        )))
    })?;
    let config = harness.spec().asks.config.as_deref().unwrap_or("-");
    let word = match outcome {
        AskHookInstall::Added => "installed",
        AskHookInstall::AlreadyThere => "already installed",
        AskHookInstall::Unsupported => "no hook to install",
    };
    println!("{}\t{word}\t{config}\t{}", args.program, hook.display());
    Ok(())
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

/// An ask id: the time, the process and a counter, so two callers in one
/// millisecond still differ.
fn ask_id(now_ms: u64) -> String {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("{now_ms}.{}.{n}", std::process::id())
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
