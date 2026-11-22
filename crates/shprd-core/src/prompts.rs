//! The standing instructions a role starts with: the words this build ships,
//! the person's installed file, and the upgrade that keeps an unedited default
//! from going stale.
//!
//! Each built-in role has a prompt the build carries (`data/templates/`): the
//! coordinator's (`assistant.md`, the name Shepherdr uses for it and the one
//! M9.3's `prompt:assistant` default names), the worker's, the reviewer's and
//! the takeover's (the opening of a handover brief; it is not a role). A role's `prompt` field is either `prompt:<id>`, which names a
//! file in the person's prompts directory, or a path.
//!
//! # Where the files live
//!
//! `$XDG_CONFIG_HOME/shprd/prompts/<id>.md`, or
//! `~/.config/shprd/prompts/<id>.md` when `XDG_CONFIG_HOME` is unset. The
//! shipped words are written there on first use ([`ensure`], which [`list`]
//! also calls), so a person can read and edit what an agent is told without
//! digging in the binary.
//!
//! # The environment is the binary's
//!
//! Nothing here reads the process environment: the configuration root is
//! resolved once, where the binary is wired to the machine ([`config_home`]
//! takes the `$XDG_CONFIG_HOME` value as an argument), and passed in as a
//! path. A test that builds its own home therefore keeps its prompts there
//! whatever the process's own XDG settings say, and two tests can never write
//! into the same real directory.
//!
//! # The upgrade rule
//!
//! A shipped default changes between builds; an installed file does not. So a
//! file is replaced with today's words **only** when it is byte-identical to a
//! default an earlier build shipped ([`ShippedPrompt::old`] holds their
//! SHA-256 digests). An unedited default is therefore never stale, and an edit
//! is never lost: a file that matches neither today's words nor a retired one
//! is the person's, and every command leaves it alone. [`reset`] is the only
//! way to put the shipped words back over an edit, and it is explicit.
//!
//! # Borrowed behaviour
//!
//! The prompt files, the first-use write and the "upgrade only an untouched
//! default" rule follow Shepherdr (`eliasstravik/shepherdr`,
//! `mods/tasks/bin/herdr_task/templates.py` — `instructions()` and
//! `OLD_WORKERS` — and `templates/{assistant,worker,reviewer}.md`, commit
//! a3f327a, MIT). The words themselves are adapted from those files for
//! shprd's verbs; the Rust shape is shprd's own.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

/// One prompt this build ships.
#[derive(Debug, Clone, Copy)]
pub struct ShippedPrompt {
    /// The id the file is installed under, without `.md`.
    pub id: &'static str,
    /// The role this prompt belongs to by default.
    pub role: &'static str,
    /// One line about the role, for `shprd prompts list`.
    pub about: &'static str,
    /// The words themselves.
    pub text: &'static str,
    /// SHA-256 digests (lowercase hex) of defaults earlier builds installed
    /// under this id. A file matching one of them is upgraded; a file matching
    /// none is the person's own.
    pub old: &'static [&'static str],
}

/// The first worker wording this build installed, retired once the shprd verbs
/// settled. A development build of M9.4 wrote it, so a file byte-identical to
/// it must not be left behind; the text is kept under `data/templates/history/`
/// for the test that proves [`WORKER_FIRST`]'s digest is the one listed.
#[cfg(test)]
const WORKER_FIRST: &str = include_str!("../../../data/templates/history/worker-first.md");

/// The SHA-256 of [`WORKER_FIRST`], the worker's first retired default.
const WORKER_FIRST_DIGEST: &str =
    "4751c29cd19041fe050ad5d8fbda4b28e4ea783a22ae99ff1cfe177e1a1ed547";

/// The worker wording M9.4 shipped, which told the worker to start its own
/// reviewer. The engine reviews on hand-in now, so a file byte-identical to it
/// is upgraded too; the text is kept under `data/templates/history/` for the
/// test that proves [`WORKER_M9`]'s digest is the one listed.
#[cfg(test)]
const WORKER_M9: &str = include_str!("../../../data/templates/history/worker-m9.md");

/// The SHA-256 of [`WORKER_M9`], the worker's second retired default.
const WORKER_M9_DIGEST: &str = "900f82be310d6ad3f688dc4276a9bd7bc0817ae18310a331960c880b50dd0872";

/// The worker wording M9.5 shipped, which told every worker to ask with its
/// CLI's own question tool. A program with no ask hook cannot raise a card that
/// way, so today's wording sends the worker to the `Asking the person` section
/// of its brief, which names the route from the program's harness.
#[cfg(test)]
const WORKER_M9_ASKS: &str = include_str!("../../../data/templates/history/worker-m9-asks.md");

/// The SHA-256 of [`WORKER_M9_ASKS`], the worker's third retired default.
const WORKER_M9_ASKS_DIGEST: &str =
    "59e22b0f71d7810c737e8ead9937852cd1166f3867653aea7fea92970081583f";

/// The worker wording M9.6 shipped, which put the proof rule after the hand-in
/// rule and sent the proof path into the summary. A hand-in is spent once, so
/// today's words put the rule first and give the path its own `--proof` flag.
#[cfg(test)]
const WORKER_M9_PROOF: &str = include_str!("../../../data/templates/history/worker-m9-proof.md");

/// The SHA-256 of [`WORKER_M9_PROOF`], the worker's fourth retired default.
const WORKER_M9_PROOF_DIGEST: &str =
    "a371c687e291a4ed26386320b8f5bb9f08adf6764d459e7055e75edaa78ebcf8";

/// The worker wording M10.0 shipped, which named the proof's `--proof` flag but
/// not where the artifact has to live. A proof outside the worktree is one the
/// reviewer cannot open, so today's words say the artifact must be inside it.
#[cfg(test)]
const WORKER_M10_PROOF: &str = include_str!("../../../data/templates/history/worker-m10-proof.md");

/// The SHA-256 of [`WORKER_M10_PROOF`], the worker's fifth retired default.
const WORKER_M10_PROOF_DIGEST: &str =
    "39c27a0d0d261505d9cf2663e11c4f724c9a33f52ce826a6bbf4d9b65895eb50";

/// The worker wording before it opened with an identity line. With another
/// product's session note in the same CLI (Shepherdr's), an agent could answer
/// as that product, so today's words say first who it is.
#[cfg(test)]
const WORKER_PRE_IDENTITY: &str =
    include_str!("../../../data/templates/history/worker-pre-identity.md");

/// The SHA-256 of [`WORKER_PRE_IDENTITY`].
const WORKER_PRE_IDENTITY_DIGEST: &str =
    "4349b51d144d5377b3036cbf48c7af6a9e3af29db67e27a090641fde320116ba";

/// The worker's retired defaults.
const WORKER_OLD: &[&str] = &[
    WORKER_FIRST_DIGEST,
    WORKER_M9_DIGEST,
    WORKER_M9_ASKS_DIGEST,
    WORKER_M9_PROOF_DIGEST,
    WORKER_M10_PROOF_DIGEST,
    WORKER_PRE_IDENTITY_DIGEST,
];

/// The backup wording M9.5 shipped, retired for the same reason as the worker's:
/// the ask route is the program's, not one CLI tool's.
#[cfg(test)]
const BACKUP_M9_ASKS: &str = include_str!("../../../data/templates/history/backup-m9-asks.md");

/// The SHA-256 of [`BACKUP_M9_ASKS`], the backup's first retired default.
const BACKUP_M9_ASKS_DIGEST: &str =
    "870902264b1bcf1c89e6fddb66dd04728fcfca510aa0722c639d1236023670f4";

/// The backup's retired defaults.
const BACKUP_OLD: &[&str] = &[BACKUP_M9_ASKS_DIGEST];

/// The coordinator wording M9.5 shipped, retired because it named only the
/// CLI's own question tool, which a program with no ask hook does not have.
#[cfg(test)]
const ASSISTANT_M9_ASKS: &str =
    include_str!("../../../data/templates/history/assistant-m9-asks.md");

/// The SHA-256 of [`ASSISTANT_M9_ASKS`], the coordinator's first retired
/// default.
const ASSISTANT_M9_ASKS_DIGEST: &str =
    "55fc1fde127f7208667ccf62caaa6d5c57f6a3678a74d78fc8ea4583fce8ebb3";

/// The coordinator wording this task shipped first, which sent a coordinator on
/// a program with no ask hook to `shprd ask raise`. That verb needs a task, and
/// a coordinator's launch carries none (`SHPRD_TASK` is set only for a task), so
/// the route it named could not work for the program it was written for. Today's
/// words use `shprd agent escalate` for a run-wide question instead.
#[cfg(test)]
const ASSISTANT_ASKS_RAISE: &str =
    include_str!("../../../data/templates/history/assistant-asks-raise.md");

/// The SHA-256 of [`ASSISTANT_ASKS_RAISE`], the coordinator's second retired
/// default.
const ASSISTANT_ASKS_RAISE_DIGEST: &str =
    "5759b6e936356e985db7ef39f21037b9e68a2d0a955714be3bd65c1d327af027";

/// The assistant wording before it opened with an identity line. With another
/// product's session note in the same CLI (Shepherdr's), an agent could answer
/// as that product, so today's words say first who it is.
#[cfg(test)]
const ASSISTANT_PRE_IDENTITY: &str =
    include_str!("../../../data/templates/history/assistant-pre-identity.md");

/// The SHA-256 of [`ASSISTANT_PRE_IDENTITY`].
const ASSISTANT_PRE_IDENTITY_DIGEST: &str =
    "41298d28d3c74f5dfb9844e5ae33254dfc940a61e3b327b1411e83498b46e185";

/// The coordinator wording that opened with its identity but only said "never
/// write code yourself": a Sonnet coordinator asked "add a hello.py that prints
/// hi" wrote the file. Today's words say every ask that changes the repository
/// is a task, however it is worded.
#[cfg(test)]
const ASSISTANT_M11_IDENTITY: &str =
    include_str!("../../../data/templates/history/assistant-m11-identity.md");

/// The SHA-256 of [`ASSISTANT_M11_IDENTITY`].
const ASSISTANT_M11_IDENTITY_DIGEST: &str =
    "7845593091f7af65662ce6df93571183751c0d5e15be1d1ec3438a0a999839c3";

/// The coordinator wording that made every ask a task but did not say how the
/// person accepts one: a coordinator told the person to run `shprd approve`.
#[cfg(test)]
const ASSISTANT_M11_TASKS_ONLY: &str =
    include_str!("../../../data/templates/history/assistant-m11-tasks-only.md");

/// The SHA-256 of [`ASSISTANT_M11_TASKS_ONLY`].
const ASSISTANT_M11_TASKS_ONLY_DIGEST: &str =
    "09e6b9146233e0219a4718d4782707c71accaa030743a15096b7d88a7c415a50";

/// The coordinator wording that said to ignore another product's notes but not
/// to never run its tools: a coordinator made tasks with `herdr-task`, which
/// shprd's panes never showed (SHPR-79).
#[cfg(test)]
const ASSISTANT_M11_ACCEPT: &str =
    include_str!("../../../data/templates/history/assistant-m11-accept.md");

/// The SHA-256 of [`ASSISTANT_M11_ACCEPT`].
const ASSISTANT_M11_ACCEPT_DIGEST: &str =
    "2edb55677e9ae75377a852243658a791b0169d71c55b8bd711c7192f1aa00a50";

/// The coordinator wording shipped before SHPR-83 (`git show
/// 7bc01e7^:data/templates/assistant.md`). An install of it read as `edited`
/// because its digest was never listed, so it never upgraded.
#[cfg(test)]
const ASSISTANT_PRE_SHPR83: &str =
    include_str!("../../../data/templates/history/assistant-pre-shpr83.md");

/// The SHA-256 of [`ASSISTANT_PRE_SHPR83`].
const ASSISTANT_PRE_SHPR83_DIGEST: &str =
    "b2bb0172dfa307a6f7e93f3a2883b3c00c99f21a836d25772a86a2d1d24340a5";

/// The coordinator's retired defaults.
const ASSISTANT_OLD: &[&str] = &[
    ASSISTANT_M9_ASKS_DIGEST,
    ASSISTANT_ASKS_RAISE_DIGEST,
    ASSISTANT_PRE_IDENTITY_DIGEST,
    ASSISTANT_M11_IDENTITY_DIGEST,
    ASSISTANT_M11_TASKS_ONLY_DIGEST,
    ASSISTANT_M11_ACCEPT_DIGEST,
    ASSISTANT_PRE_SHPR83_DIGEST,
];

/// The reviewer wording M9.6 shipped, which did not name the asks the packet
/// now carries. A criterion about asking is only checkable from those, so
/// today's words say to use them.
#[cfg(test)]
const REVIEWER_M9_ASKS: &str = include_str!("../../../data/templates/history/reviewer-m9-asks.md");

/// The SHA-256 of [`REVIEWER_M9_ASKS`], the reviewer's first retired default.
const REVIEWER_M9_ASKS_DIGEST: &str =
    "3d9ee44b1a18395e0463945ab0d34baed819d8ec7ad8fa3896491ec0120a517c";

/// The reviewer wording before it opened with an identity line. With another
/// product's session note in the same CLI (Shepherdr's), an agent could answer
/// as that product, so today's words say first who it is.
#[cfg(test)]
const REVIEWER_PRE_IDENTITY: &str =
    include_str!("../../../data/templates/history/reviewer-pre-identity.md");

/// The SHA-256 of [`REVIEWER_PRE_IDENTITY`].
const REVIEWER_PRE_IDENTITY_DIGEST: &str =
    "2c28ab6907cffc0e9d2173d49042a0918f4f7c744ad8be2183cfd752d51c291e";

/// The reviewer's retired defaults.
const REVIEWER_OLD: &[&str] = &[REVIEWER_M9_ASKS_DIGEST, REVIEWER_PRE_IDENTITY_DIGEST];

/// The prompts this build ships, in the order `shprd prompts list` prints them.
pub const SHIPPED: [ShippedPrompt; 4] = [
    ShippedPrompt {
        id: "assistant",
        role: "coordinator",
        about: "Plans the work and starts a task for each part.",
        text: include_str!("../../../data/templates/assistant.md"),
        old: ASSISTANT_OLD,
    },
    ShippedPrompt {
        id: "worker",
        role: "worker",
        about: "Does the work of one task on its own branch.",
        text: include_str!("../../../data/templates/worker.md"),
        old: WORKER_OLD,
    },
    ShippedPrompt {
        id: "reviewer",
        role: "reviewer",
        about: "Reads a finished attempt's diff and decides.",
        text: include_str!("../../../data/templates/reviewer.md"),
        old: REVIEWER_OLD,
    },
    ShippedPrompt {
        // Not a role: the opening a takeover brief carries, when the worker's
        // program cannot go on and the next line of the worker chain takes over.
        // The id stays `backup` so an installed (or edited) file keeps its name.
        id: "backup",
        role: "takeover",
        about: "Opens the brief of a program that takes a task over.",
        text: include_str!("../../../data/templates/backup.md"),
        old: BACKUP_OLD,
    },
];

/// Every prompt this build ships.
pub fn shipped() -> &'static [ShippedPrompt] {
    &SHIPPED
}

/// The shipped prompt `id` names, if one is shipped under it or under its alias.
pub fn shipped_prompt(id: &str) -> Option<&'static ShippedPrompt> {
    let id = canonical(id);
    SHIPPED.iter().find(|prompt| prompt.id == id)
}

/// The id a prompt is stored under.
///
/// The coordinator's words are `assistant.md`: that is the name Shepherdr gives
/// them and the one M9.3's default `prompt:assistant` names, so `coordinator` is
/// accepted as the same prompt rather than a second file.
pub fn canonical(id: &str) -> &str {
    match id {
        "coordinator" => "assistant",
        other => other,
    }
}

/// The prompts directory under `config_home`: `<config_home>/shprd/prompts`.
///
/// The root itself comes from [`config_home`], resolved once by the caller, so a
/// library never consults the process environment for it.
pub fn prompts_dir(config_home: &Path) -> PathBuf {
    config_home.join("shprd").join("prompts")
}

/// The configuration root for `home`: `xdg_config_home` (the `$XDG_CONFIG_HOME`
/// the caller read) when it names a directory, else `<home>/.config`.
///
/// The value is an argument rather than an environment lookup so that the one
/// place that reads `$XDG_CONFIG_HOME` is the binary's wiring, and every test
/// can state the answer it wants. An empty value is unset, which is what the
/// XDG specification says.
pub fn config_home(home: &Path, xdg_config_home: Option<&OsStr>) -> PathBuf {
    match xdg_config_home.filter(|value| !value.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home.join(".config"),
    }
}

/// The file `id` is stored in, inside `dir`.
pub fn path_of(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{}.md", canonical(id)))
}

/// Refuse an id that is not one a file name may carry.
pub fn check_id(id: &str) -> Result<(), PromptError> {
    if id.is_empty() {
        return Err(PromptError::BadId { id: id.to_owned() });
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(PromptError::BadId { id: id.to_owned() });
    }
    Ok(())
}

/// What an installed file is, compared with what this build ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The file holds exactly the words this build ships.
    Default,
    /// The file holds the person's own words, or an older build's untracked
    /// default, and is left alone.
    Edited,
}

impl State {
    /// The name the verb's output and the API use.
    pub fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Edited => "edited",
        }
    }
}

/// What one command did to the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The file did not exist and the shipped words were written.
    Created,
    /// The file was a retired default and today's words replaced it.
    Upgraded,
    /// The file is the person's, or already current; nothing was written.
    Kept,
    /// The text the caller gave was written.
    Written,
    /// The text the caller gave was already there.
    Unchanged,
    /// The shipped words were put back over an edit.
    Reset,
}

impl Change {
    /// The name the verb's output and the API use.
    pub fn name(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Upgraded => "upgraded",
            Self::Kept => "kept",
            Self::Written => "written",
            Self::Unchanged => "unchanged",
            Self::Reset => "reset",
        }
    }
}

/// One prompt as it stands on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// The id it is stored under.
    pub id: String,
    /// The file.
    pub path: PathBuf,
    /// The words.
    pub text: String,
    /// Whether the file is the shipped default or the person's own.
    pub state: State,
    /// What the command that produced this did.
    pub change: Change,
    /// One line about the role, when this build ships this prompt.
    pub about: Option<&'static str>,
    /// Whether this build ships a prompt under this id.
    pub shipped: bool,
}

impl Installed {
    /// The one line `shprd prompts list` prints.
    pub fn line(&self) -> String {
        let about = self.about.unwrap_or("a prompt of your own");
        format!(
            "{:<10} {:<8} {} — {about}",
            self.id,
            self.state.name(),
            self.path.display()
        )
    }
}

/// The shipped prompt `id` names, written on first use and upgraded when it is
/// an untouched older default.
///
/// A custom id (one no build ships) is read if its file exists and refused with
/// the file's path when it does not: there are no words for shprd to invent.
pub fn ensure(dir: &Path, id: &str) -> Result<Installed, PromptError> {
    let old: &[&str] = shipped_prompt(id).map(|prompt| prompt.old).unwrap_or(&[]);
    ensure_with(dir, id, old)
}

/// [`ensure`], with the retired defaults given by the caller.
///
/// The default list is [`ShippedPrompt::old`]; a caller that is not looking at
/// the shipped table (a test proving the upgrade rule) passes its own.
pub fn ensure_with(dir: &Path, id: &str, old: &[&str]) -> Result<Installed, PromptError> {
    check_id(id)?;
    let path = path_of(dir, id);
    let shipped = shipped_prompt(id);
    match std::fs::read(&path) {
        Ok(bytes) => {
            if let Some(prompt) = shipped
                && bytes == prompt.text.as_bytes()
            {
                return Ok(installed(
                    prompt,
                    dir,
                    prompt.text.to_owned(),
                    State::Default,
                    Change::Kept,
                ));
            }
            if shipped.is_some() && old.iter().any(|hash| *hash == digest(&bytes)) {
                let prompt = shipped.ok_or_else(|| unknown(id, &path))?;
                write_text(&path, prompt.text)?;
                return Ok(installed(
                    prompt,
                    dir,
                    prompt.text.to_owned(),
                    State::Default,
                    Change::Upgraded,
                ));
            }
            let text = utf8(&path, bytes)?;
            let state = match shipped {
                Some(prompt) if text == prompt.text => State::Default,
                _ => State::Edited,
            };
            Ok(match shipped {
                Some(prompt) => installed(prompt, dir, text, state, Change::Kept),
                None => Installed {
                    id: canonical(id).to_owned(),
                    path,
                    text,
                    state,
                    change: Change::Kept,
                    about: None,
                    shipped: false,
                },
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match shipped {
            Some(prompt) => {
                write_text(&path, prompt.text)?;
                Ok(installed(
                    prompt,
                    dir,
                    prompt.text.to_owned(),
                    State::Default,
                    Change::Created,
                ))
            }
            None => Err(unknown(id, &path)),
        },
        Err(source) => Err(PromptError::Read { path, source }),
    }
}

/// Put the shipped words back over whatever the file holds.
///
/// This is the one command that replaces an edit, which is why it is a verb of
/// its own (`shprd prompts reset`) and never happens behind the person's back.
pub fn reset(dir: &Path, id: &str) -> Result<Installed, PromptError> {
    check_id(id)?;
    let prompt = shipped_prompt(id).ok_or_else(|| unknown(id, &path_of(dir, id)))?;
    let path = path_of(dir, id);
    let change = match std::fs::read(&path) {
        Ok(bytes) if bytes == prompt.text.as_bytes() => Change::Unchanged,
        Ok(_) | Err(_) => {
            write_text(&path, prompt.text)?;
            Change::Reset
        }
    };
    Ok(installed(
        prompt,
        dir,
        prompt.text.to_owned(),
        State::Default,
        change,
    ))
}

/// Write the person's own words over the file, making it if it does not exist.
///
/// The write is what a prompt editor's save does (`PUT /api/prompts/<id>`): the
/// person's text always wins, because they asked for it.
pub fn write(dir: &Path, id: &str, text: &str) -> Result<Installed, PromptError> {
    check_id(id)?;
    let path = path_of(dir, id);
    let shipped = shipped_prompt(id);
    let change = match std::fs::read(&path) {
        Ok(bytes) if bytes == text.as_bytes() => Change::Unchanged,
        Ok(_) | Err(_) => {
            write_text(&path, text)?;
            Change::Written
        }
    };
    let state = match shipped {
        Some(prompt) if text == prompt.text => State::Default,
        _ => State::Edited,
    };
    Ok(match shipped {
        Some(prompt) => installed_at(prompt, path, text.to_owned(), state, change),
        None => Installed {
            id: canonical(id).to_owned(),
            path,
            text: text.to_owned(),
            state,
            change,
            about: None,
            shipped: false,
        },
    })
}

/// One prompt [`list`] found: the words, or the file it could not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listed {
    /// A prompt, as [`ensure`] reads it.
    Prompt(Installed),
    /// A file in the prompts directory that cannot be used, with the reason: a
    /// person's file is never dropped from the list without a word about why.
    Unreadable {
        /// The id its file name names.
        id: String,
        /// The file.
        path: PathBuf,
        /// Why it cannot be read, in one line.
        reason: String,
    },
}

/// Read every prompt this machine has: the shipped ones, written on first use,
/// then any `.md` file of the person's own, by id.
///
/// A shipped prompt that cannot be installed is an error, because there is
/// nothing to show for it. A file of the person's own that cannot be read is
/// listed as [`Listed::Unreadable`] with its reason, so a broken file is visible
/// in the list rather than silently missing.
pub fn list(dir: &Path) -> Result<Vec<Listed>, PromptError> {
    let mut prompts = Vec::new();
    for prompt in shipped() {
        prompts.push(Listed::Prompt(ensure(dir, prompt.id)?));
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(prompts);
    };
    let mut extra: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|source| PromptError::Read {
                path: dir.to_path_buf(),
                source,
            })?
            .path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".md") else {
            continue;
        };
        if !path.is_file() || shipped_prompt(stem).is_some() {
            continue;
        }
        extra.push(path);
    }
    extra.sort();
    for path in extra {
        let Some(id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".md"))
        else {
            continue;
        };
        prompts.push(match ensure(dir, id) {
            Ok(installed) => Listed::Prompt(installed),
            Err(error) => Listed::Unreadable {
                id: id.to_owned(),
                path,
                reason: error.to_string(),
            },
        });
    }
    Ok(prompts)
}

/// A role's `prompt` value: an id in the person's prompts directory, or a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptRef {
    /// `prompt:<id>`: a file in the prompts directory.
    Prompt(String),
    /// A path, with `~` allowed.
    Path(PathBuf),
}

impl PromptRef {
    /// The prefix that marks an id rather than a path.
    pub const PREFIX: &'static str = "prompt:";

    /// Parse a role's `prompt` value.
    pub fn parse(text: &str) -> Result<PromptRef, PromptError> {
        let trimmed = text.trim();
        if let Some(id) = trimmed.strip_prefix(Self::PREFIX) {
            if id.is_empty() {
                return Err(PromptError::BadRef {
                    reference: trimmed.to_owned(),
                });
            }
            return Ok(PromptRef::Prompt(id.to_owned()));
        }
        if trimmed.is_empty() {
            return Err(PromptError::BadRef {
                reference: trimmed.to_owned(),
            });
        }
        Ok(PromptRef::Path(PathBuf::from(trimmed)))
    }

    /// The words this reference names, without a leading credit comment.
    ///
    /// An id is written on first use and upgraded like any other installed
    /// prompt; a path must exist, and a `~` in it is `home`.
    pub fn read(&self, dir: &Path, home: &Path) -> Result<String, PromptError> {
        match self {
            PromptRef::Prompt(id) => {
                ensure(dir, id).map(|prompt| strip_leading_comment(&prompt.text).to_owned())
            }
            PromptRef::Path(path) => {
                let path = expand_tilde(path, home);
                std::fs::read_to_string(&path)
                    .map(|text| strip_leading_comment(&text).to_owned())
                    .map_err(|source| PromptError::Read { path, source })
            }
        }
    }

    /// The id or path as it was written, for a message.
    pub fn text(&self) -> String {
        match self {
            PromptRef::Prompt(id) => format!("{}{id}", Self::PREFIX),
            PromptRef::Path(path) => path.display().to_string(),
        }
    }
}

/// `text` without a leading `<!-- ... -->` block (and the blank lines after it).
///
/// A template opens with a comment holding its licence credit. The file on disk
/// keeps it; the words an agent is sent never carry it. Text that does not open
/// with a closed comment is returned whole.
pub fn strip_leading_comment(text: &str) -> &str {
    let trimmed = text.trim_start();
    let Some(rest) = trimmed.strip_prefix("<!--") else {
        return text;
    };
    match rest.split_once("-->") {
        Some((_, after)) => after.trim_start_matches(['\n', '\r']),
        None => text,
    }
}

/// `path` with a leading `~` replaced by `home`.
pub fn expand_tilde(path: &Path, home: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if text == "~" => home.to_path_buf(),
        None => path.to_path_buf(),
    }
}

/// The SHA-256 of `bytes`, lowercase hex.
pub fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let sum = hasher.finalize();
    let mut hex = String::with_capacity(sum.len() * 2);
    for byte in sum {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// An [`Installed`] for a shipped prompt, from the words already read.
fn installed(
    prompt: &'static ShippedPrompt,
    dir: &Path,
    text: String,
    state: State,
    change: Change,
) -> Installed {
    installed_at(prompt, path_of(dir, prompt.id), text, state, change)
}

/// [`installed`], with the path given rather than derived.
fn installed_at(
    prompt: &'static ShippedPrompt,
    path: PathBuf,
    text: String,
    state: State,
    change: Change,
) -> Installed {
    Installed {
        id: prompt.id.to_owned(),
        path,
        text,
        state,
        change,
        about: Some(prompt.about),
        shipped: true,
    }
}

/// The refusal for an id this build ships nothing for and no file exists for.
fn unknown(id: &str, path: &Path) -> PromptError {
    let known = shipped()
        .iter()
        .map(|prompt| prompt.id)
        .collect::<Vec<_>>()
        .join(", ");
    PromptError::Unknown {
        id: canonical(id).to_owned(),
        known,
        path: path.to_path_buf(),
    }
}

/// The text of a file, or the refusal saying it is not text.
fn utf8(path: &Path, bytes: Vec<u8>) -> Result<String, PromptError> {
    String::from_utf8(bytes).map_err(|_| PromptError::Encoding {
        path: path.to_path_buf(),
    })
}

/// Write `text` to `path`, making the directory and replacing the file whole.
///
/// The temp file and the rename are what keep a reader from seeing half a
/// prompt; the temp name is ignored by [`list`], which reads only `.md`.
fn write_text(path: &Path, text: &str) -> Result<(), PromptError> {
    let write_error = |source| PromptError::Write {
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(write_error)?;
    }
    let temp = path.with_extension("md.tmp");
    std::fs::write(&temp, text).map_err(write_error)?;
    std::fs::rename(&temp, path).map_err(write_error)
}

/// Why a prompt could not be read, written or found.
#[derive(Debug, Error)]
pub enum PromptError {
    /// The id could name a directory or a file outside the prompts directory.
    #[error("`{id}` is not a prompt id: an id is letters, digits, `-` or `_`")]
    BadId {
        /// The id as it was written.
        id: String,
    },
    /// Nothing ships this prompt and no file exists for it.
    #[error(
        "no prompt `{id}`: this build ships {known}; write {path}, or point the role's `prompt` at a path"
    )]
    Unknown {
        /// The id as it was asked for.
        id: String,
        /// The ids this build ships.
        known: String,
        /// The file the id would be stored in.
        path: PathBuf,
    },
    /// A prompt file could not be read.
    #[error("cannot read {path}")]
    Read {
        /// The file that failed.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A prompt file could not be written.
    #[error("cannot write {path}")]
    Write {
        /// The file that failed.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A file that is not UTF-8 text cannot be a prompt.
    #[error("{path} is not UTF-8 text, so it cannot be a prompt")]
    Encoding {
        /// The file that is not text.
        path: PathBuf,
    },
    /// A role's `prompt` value is neither `prompt:<id>` nor a path.
    #[error("`{reference}` is neither `prompt:<id>` nor a path")]
    BadRef {
        /// The value as it was written.
        reference: String,
    },
}

#[cfg(test)]
mod tests;
