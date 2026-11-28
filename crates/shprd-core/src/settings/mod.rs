//! The settings model: roles as a keyed table, layers, and where each value came
//! from.
//!
//! The person's file is `~/.config/shprd/settings.toml` (version 1). It holds
//! their own defaults at the top level and one entry per repository:
//!
//! ```toml
//! version = 1
//!
//! [roles]
//! order = ["coordinator", "worker", "reviewer", "docs"]
//!
//! [roles.coordinator]
//! title = "Assistant"                     # the word the person sees
//! about = "Plans the work and starts a task for each part."
//! kind = "coordinator"                    # coordinator | worker | reviewer | helper
//! prompt = "prompt:assistant"             # "prompt:<id>" or a path
//! agent = "claude"                        # a harness id from data/harness/
//! model = "claude-opus-5-5"
//! effort = "high"
//! fast = false
//! perm = "auto"                           # one of that harness's [permissions] ids
//! fallback = ["codex", "pi"]              # programs, in order
//!
//! [roles.docs]
//! title = "Docs writer"
//! about = "Writes and fixes documentation only."
//! kind = "worker"
//! prompt = "~/.config/shprd/prompts/docs.md"
//! agent = "pi"
//!
//! [tasks]
//! merge = "checks"
//! review_rounds = 2
//!
//! [review]
//! cross_vendor = true
//!
//! [repos."shprd"]                          # the same keys, for one repository
//! [repos."shprd".tasks]
//! merge = "auto"
//! ```
//!
//! Roles are keyed by id and the three built-ins (`coordinator`, `worker`,
//! `reviewer`) are only the first three rows: any id is a role, with
//! whatever fields it names. `roles.order` is the order a settings UI shows
//! them in.
//!
//! A repository may also commit a small file at `<repo>/.shprd/settings.toml`.
//! It may set only [`REPO_KEYS`], because code an agent wrote must never widen
//! its own guards: `roles`, `agents`, permissions and the review rules are not
//! settable there.
//!
//! # Resolution
//!
//! One rule, one function ([`resolve`]), nearest answer wins:
//!
//! ```text
//! the task's own record  >  your entry for that repo  >  you  >  the repo's committed file  >  built in
//! ```
//!
//! Every value is returned with the layer it came from ([`Source`]), because the
//! brief's Settings block says who set each line. A key no layer names is
//! reported as unset ([`Row`]) and resolves to [`builtin`]'s value.
//!
//! Resolution is per key, roles included: [`Key::Role`] is `roles.<id>.<field>`,
//! so setting `roles.worker.agent` for one repository leaves the rest of that
//! role inherited, which is what makes a Settings block able to name a source
//! per line.
//!
//! # Writing
//!
//! [`Loaded::write`] applies the same rule a settings page needs: a value equal
//! to the one the scope would inherit is not stored at all, so "set it back" and
//! "reset it" are the same edit. The file is edited rather than rewritten (see
//! [`store`]), and a write refuses when the file changed between being read and
//! being written, so two writers — a page and a terminal — never silently
//! replace each other's work.
//!
//! # Keys the old config file still sets
//!
//! `~/.config/shprd/config.toml` predates this model and keeps working: its
//! `[land] merge`, `[tick] max_running`, `[tick] interval_secs`,
//! `[janitor] archive_days`, `[janitor] namespace`, `[review] must_fix` and
//! `[review] rounds` project onto the same `tasks.*` keys, and its
//! `[roster] reviewer` (with the reviewer's `[roster.effort]` entry) projects
//! onto `roles.reviewer.agent`, `roles.reviewer.model` and
//! `roles.reviewer.effort` (see
//! [`crate::config::ConfigLayer::settings_projection`]). That layer sits under
//! the settings file — under your own defaults and under your entry for a
//! repository, because it is the same person's older file — and every value it
//! supplies is reported as [`Source::Legacy`], so a person who has not migrated
//! loses nothing and can see what still comes from the old file. The old
//! spellings are also accepted as key names on the command line
//! ([`Key::parse`]).
//!
//! Borrowed behaviour: the layered resolution, the reserved built-in rows, the
//! "a value equal to the inherited one is stored nowhere" rule and the keyed
//! role table follow Shepherdr (`eliasstravik/shepherdr`, `herdr_task/settings.py`,
//! `core/settings.js` and `mods/shepherdr-settings/main.js`, commit a3f327a,
//! MIT). The Rust shape is shprd's own.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::config::{Config, ConfigLayer, MergePolicy};
use crate::event::Severity;

/// The settings format this build writes and reads.
pub const VERSION: u32 = 1;

/// The role fields, in the order a settings UI and `settings show` list them.
///
/// Order matters only for display: every field resolves on its own.
pub const ROLE_FIELDS: [RoleField; 12] = [
    RoleField::Title,
    RoleField::About,
    RoleField::Kind,
    RoleField::Prompt,
    RoleField::Agent,
    RoleField::Model,
    RoleField::Effort,
    RoleField::Fast,
    RoleField::Perm,
    RoleField::Agents,
    RoleField::Fallback,
    RoleField::Readonly,
];

/// The role ids this build ships, in their default order.
pub const BUILTIN_ROLES: [&str; 3] = ["coordinator", "worker", "reviewer"];

/// Role ids an earlier build shipped and this one does not: a file may still
/// name them (`roles.backup.agent`), and they are read as nothing — never a
/// custom row, never an error. The Backup agent went when the worker's program
/// lines became a fallback chain.
pub const RETIRED_ROLES: [&str; 1] = ["backup"];

/// The keys a repository's committed `.shprd/settings.toml` may set.
///
/// The list is a guard, not a style rule: a repository's file is written by
/// whoever works in the repository, agents included, so it may describe how the
/// repository is worked on (its base branch, its branch names, the words its
/// tasks start and finish with) and never how strictly shprd guards it.
pub const REPO_KEYS: [TaskKey; 4] = [
    TaskKey::Base,
    TaskKey::Branch,
    TaskKey::StartNotes,
    TaskKey::FinishNotes,
];

/// Why a repository's committed file may not set a key.
const REPO_LIMIT: &str = "a repository's committed settings file may only set `tasks.base`, \
     `tasks.branch`, `tasks.start_notes` and `tasks.finish_notes`, so code an agent wrote cannot \
     widen its own guards";

/// The hint an unknown key's error carries.
const KEY_HINT: &str = "keys are `roles.order`, `roles.<id>.<field>`, `tasks.<key>`, `graph.dagr`, \
     `review.cross_vendor`, `host` and `web.<key>`";

/// Where one layer of settings sits in the resolution chain, nearest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// The task's own record: what the engine launched this task with.
    Task,
    /// The person's entry for this repository.
    RepoEntry,
    /// The person's own defaults.
    You,
    /// The person's pre-M9 `config.toml`: the keys the old file still sets.
    Legacy,
    /// The repository's committed `.shprd/settings.toml`.
    RepoFile,
    /// The values this build ships.
    BuiltIn,
}

impl Source {
    /// The words a settings line uses for this layer.
    pub fn label(self) -> &'static str {
        match self {
            Self::Task => "this task",
            Self::RepoEntry => "this repo",
            Self::You => "you",
            Self::Legacy => "your old config file",
            Self::RepoFile => "the repo's file",
            Self::BuiltIn => "built in",
        }
    }

    /// The name the API and the JSON output use.
    pub fn name(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::RepoEntry => "repo_entry",
            Self::You => "you",
            Self::Legacy => "legacy",
            Self::RepoFile => "repo_file",
            Self::BuiltIn => "built_in",
        }
    }
}

/// One settings value, typed by what the key accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A word, a model name, a path or a line of text.
    Text(String),
    /// A whole number.
    Number(i64),
    /// A boolean.
    Switch(bool),
    /// An ordered list of words, e.g. `fallback` or `roles.order`.
    List(Vec<String>),
}

impl Value {
    /// The value as a settings line prints it.
    ///
    /// Text is quoted, so an empty default reads as `""` rather than as
    /// nothing; a list is quoted item by item.
    pub fn display(&self) -> String {
        match self {
            Self::Text(text) => format!("{text:?}"),
            Self::Number(number) => number.to_string(),
            Self::Switch(on) => on.to_string(),
            Self::List(items) => {
                let quoted: Vec<String> = items.iter().map(|item| format!("{item:?}")).collect();
                format!("[{}]", quoted.join(", "))
            }
        }
    }

    /// The value as JSON, for the API and `--json`.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Text(text) => serde_json::Value::String(text.clone()),
            Self::Number(number) => serde_json::Value::from(*number),
            Self::Switch(on) => serde_json::Value::Bool(*on),
            Self::List(items) => serde_json::Value::Array(
                items
                    .iter()
                    .cloned()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        }
    }

    /// The value a JSON document carries, when it is one a key can take.
    pub fn from_json(json: &serde_json::Value) -> Option<Value> {
        match json {
            serde_json::Value::String(text) => Some(Self::Text(text.clone())),
            serde_json::Value::Bool(on) => Some(Self::Switch(*on)),
            serde_json::Value::Number(number) => number.as_i64().map(Self::Number),
            serde_json::Value::Array(items) => {
                let mut words = Vec::with_capacity(items.len());
                for item in items {
                    words.push(item.as_str()?.to_owned());
                }
                Some(Self::List(words))
            }
            _ => None,
        }
    }

    /// The text of a [`Value::Text`], or [`None`].
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    /// The number of a [`Value::Number`], or [`None`].
    pub fn number(&self) -> Option<i64> {
        match self {
            Self::Number(number) => Some(*number),
            _ => None,
        }
    }

    /// The state of a [`Value::Switch`], or [`None`].
    pub fn switch(&self) -> Option<bool> {
        match self {
            Self::Switch(on) => Some(*on),
            _ => None,
        }
    }

    /// The items of a [`Value::List`], or [`None`].
    pub fn list(&self) -> Option<&[String]> {
        match self {
            Self::List(items) => Some(items),
            _ => None,
        }
    }
}

/// What a key accepts, so a value can be checked before it is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Any text, including an empty one.
    Text,
    /// A whole number, in the key's range when it has one.
    Number,
    /// `true` or `false`.
    Switch,
    /// A list of words.
    List,
    /// One of a fixed set of words.
    OneOf(&'static [&'static str]),
}

impl Kind {
    /// The words an error uses for this kind.
    pub fn expects(self) -> String {
        match self {
            Self::Text => "takes text".to_owned(),
            Self::Number => "takes a whole number".to_owned(),
            Self::Switch => "takes true or false".to_owned(),
            Self::List => "takes a list of words".to_owned(),
            Self::OneOf(words) => format!("takes one of {}", words.join(", ")),
        }
    }
}

/// The merge policies, in the words a settings file uses.
pub const MERGE_WORDS: [&str; 3] = ["ask", "checks", "auto"];

/// The severities a review must fix, in the words a settings file uses.
pub const MUST_FIX_WORDS: [&str; 4] = ["critical", "major", "minor", "trivial"];

/// The kinds of role, which decide who may dispatch and who may review.
pub const ROLE_KINDS: [&str; 4] = ["coordinator", "worker", "reviewer", "helper"];

/// The hosts shprd can drive, in the words a settings file uses.
///
/// `auto` is the default and the only spelling that decides at run time: it
/// picks Herdr desktop when a herdr-core answers for this session *and* a Herdr
/// window is attached to it, and the herdr TUI otherwise. The other three name
/// one host and leave no room for a guess.
pub const HOST_WORDS: [&str; 4] = ["auto", "herdr-desktop", "herdr", "tuios"];

/// Which terminal host shprd drives.
///
/// It is a machine setting, not a repository one: which multiplexer a person
/// works in is a fact about their screen, so a repository's committed settings
/// file may not set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum HostChoice {
    /// Pick the host: Herdr desktop when its core and window answer, else the
    /// TUI.
    #[default]
    Auto,
    /// The Herdr window, driven through herdr-core and the window's own
    /// command line.
    HerdrDesktop,
    /// The herdr TUI, driven through the `herdr` command line.
    Herdr,
    /// The tuios TUI, through its herdr-compatible shim.
    Tuios,
}

impl HostChoice {
    /// The word a settings file stores this choice as.
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::HerdrDesktop => "herdr-desktop",
            Self::Herdr => "herdr",
            Self::Tuios => "tuios",
        }
    }

    /// The choice a word names, or [`None`] when it names no host.
    pub fn parse(word: &str) -> Option<HostChoice> {
        match word {
            "auto" => Some(Self::Auto),
            "herdr-desktop" => Some(Self::HerdrDesktop),
            "herdr" => Some(Self::Herdr),
            "tuios" => Some(Self::Tuios),
            _ => None,
        }
    }

    /// Whether this choice is decided at run time rather than named.
    pub fn is_auto(self) -> bool {
        self == Self::Auto
    }
}

impl std::fmt::Display for HostChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// One field of a role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RoleField {
    /// The word the person sees.
    Title,
    /// One line about what the role does.
    About,
    /// Who may dispatch or review as this role.
    Kind,
    /// The instructions: `prompt:<id>` or a path.
    Prompt,
    /// The program, as a harness id.
    Agent,
    /// The model, in the program's own words.
    Model,
    /// The effort level, in the program's own words.
    Effort,
    /// Whether the program's fast mode is on.
    Fast,
    /// The access mode, as one of the program's `[permissions]` ids.
    Perm,
    /// The worker lines after the first, in order: one `agent|model|effort|fast|perm`
    /// string each (see [`WorkerLine`]).
    Agents,
    /// The programs to fall back through, in order. For a worker this is the
    /// pre-SHPR-44 spelling of "more lines": it still reads, as lines with the
    /// program's own defaults, when `agents` names none. A reviewer's fallback
    /// is one `program` or `program/model` entry.
    Fallback,
    /// Whether the role only reads.
    Readonly,
}

impl RoleField {
    /// The field's name in a key, e.g. `agent`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::About => "about",
            Self::Kind => "kind",
            Self::Prompt => "prompt",
            Self::Agent => "agent",
            Self::Model => "model",
            Self::Effort => "effort",
            Self::Fast => "fast",
            Self::Perm => "perm",
            Self::Agents => "agents",
            Self::Fallback => "fallback",
            Self::Readonly => "readonly",
        }
    }

    /// The field a key segment names, or [`None`].
    pub fn parse(text: &str) -> Option<RoleField> {
        ROLE_FIELDS.into_iter().find(|field| field.name() == text)
    }
}

/// What a role is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RoleKind {
    /// Plans the work and dispatches tasks.
    Coordinator,
    /// Works one task.
    Worker,
    /// Reads a finished attempt and decides.
    Reviewer,
    /// Anything else: it neither dispatches nor reviews.
    Helper,
}

impl RoleKind {
    /// The kind's name in a settings file.
    pub fn name(self) -> &'static str {
        match self {
            Self::Coordinator => "coordinator",
            Self::Worker => "worker",
            Self::Reviewer => "reviewer",
            Self::Helper => "helper",
        }
    }

    /// The kind a word names, or [`None`].
    pub fn parse(text: &str) -> Option<RoleKind> {
        match text {
            "coordinator" => Some(Self::Coordinator),
            "worker" => Some(Self::Worker),
            "reviewer" => Some(Self::Reviewer),
            "helper" => Some(Self::Helper),
            _ => None,
        }
    }
}

/// One `tasks.*` key: the workflow values, folded in from the old config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TaskKey {
    /// Who merges a ready task: `ask`, `checks` or `auto`.
    Merge,
    /// Failed review rounds allowed before a human is asked.
    ReviewRounds,
    /// The least severe finding a worker must fix.
    MustFix,
    /// Whether every task leaves one proof artifact.
    Proof,
    /// How many tasks of one repository run at once.
    MaxRunning,
    /// Days after which a done task archives itself; `0` never.
    ArchiveAfter,
    /// The namespace archived resources are put under.
    ArchiveNamespace,
    /// Whether every task gets its own worktree.
    Worktrees,
    /// The branch new tasks start from and merge into.
    Base,
    /// The pattern new tasks' branches are named after.
    Branch,
    /// Words put into a task's brief when it starts.
    StartNotes,
    /// Words put into a task's brief before it reports.
    FinishNotes,
    /// Files copied or linked from the main checkout into every worktree.
    WorktreeFiles,
    /// Seconds between two passes of the service's tick.
    TickSecs,
    /// Seconds a change must hold before the coordinator is told about it.
    NewsSettleSecs,
    /// Whether shprd rebuilds and restarts itself after a landed task changes it.
    SelfUpdate,
    /// Whether the engine removes a landed or dropped task's worktree and branch itself.
    AutoClean,
    /// The `gh` account every forge call uses; empty uses whichever is active.
    GhUser,
    /// The three-letter prefix of auto-assigned task ids; empty derives it from the repository's name.
    IdPrefix,
}

/// Every `tasks.*` key, in the order a settings UI lists them.
pub const TASK_KEYS: [TaskKey; 19] = [
    TaskKey::Merge,
    TaskKey::ReviewRounds,
    TaskKey::MustFix,
    TaskKey::Proof,
    TaskKey::MaxRunning,
    TaskKey::ArchiveAfter,
    TaskKey::ArchiveNamespace,
    TaskKey::Worktrees,
    TaskKey::Base,
    TaskKey::Branch,
    TaskKey::StartNotes,
    TaskKey::FinishNotes,
    TaskKey::WorktreeFiles,
    TaskKey::TickSecs,
    TaskKey::NewsSettleSecs,
    TaskKey::SelfUpdate,
    TaskKey::AutoClean,
    TaskKey::GhUser,
    TaskKey::IdPrefix,
];

impl TaskKey {
    /// The key's name under `[tasks]`, e.g. `max_running`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::ReviewRounds => "review_rounds",
            Self::MustFix => "must_fix",
            Self::Proof => "proof",
            Self::MaxRunning => "max_running",
            Self::ArchiveAfter => "archive_after",
            Self::ArchiveNamespace => "archive_namespace",
            Self::Worktrees => "worktrees",
            Self::Base => "base",
            Self::Branch => "branch",
            Self::StartNotes => "start_notes",
            Self::FinishNotes => "finish_notes",
            Self::WorktreeFiles => "worktree_files",
            Self::TickSecs => "tick_secs",
            Self::NewsSettleSecs => "news_settle_secs",
            Self::SelfUpdate => "self_update",
            Self::AutoClean => "auto_clean",
            Self::GhUser => "gh_user",
            Self::IdPrefix => "id_prefix",
        }
    }

    /// The key a `[tasks]` name is, or [`None`].
    pub fn parse(name: &str) -> Option<TaskKey> {
        TASK_KEYS.into_iter().find(|key| key.name() == name)
    }

    /// The words this key accepts, when it accepts a fixed set.
    fn words(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Merge => Some(&MERGE_WORDS),
            Self::MustFix => Some(&MUST_FIX_WORDS),
            _ => None,
        }
    }

    /// The range a number must lie in, when the key takes one.
    fn range(self) -> Option<(i64, i64)> {
        match self {
            Self::ReviewRounds => Some((0, 10)),
            Self::MaxRunning => Some((1, 10)),
            Self::ArchiveAfter => Some((0, 3650)),
            Self::TickSecs => Some((1, 3600)),
            Self::NewsSettleSecs => Some((0, 3600)),
            _ => None,
        }
    }

    /// Whether a repository's committed file may set this key.
    pub fn is_repo_allowed(self) -> bool {
        REPO_KEYS.contains(&self)
    }
}

/// The tunnel providers `web.tunnel` may name, in the words `shprd web up
/// --tunnel` takes.
///
/// `shprd-remote` owns the providers; this list is the words only, because the
/// settings model sits below it. A test there keeps the two the same.
pub const WEB_TUNNEL_WORDS: [&str; 5] = [
    "tailscale",
    "cloudflared",
    "cloudflared-named",
    "ngrok",
    "tunnl",
];

/// A `web.*` key: how remote access is reached.
///
/// None of these may be set by a repository's committed file: where a shell is
/// exposed is the person's decision, and a cloned repository must not make it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WebKey {
    /// The provider `shprd web up` uses when `--tunnel` is not given.
    Tunnel,
    /// tunnl.gg only: ask for the stable address.
    Stable,
    /// cloudflared-named: the tunnel's name.
    CloudflaredName,
    /// cloudflared-named: the hostname routed to it.
    CloudflaredHost,
}

/// Every `web.*` key, in the order a settings UI lists them.
pub const WEB_KEYS: [WebKey; 4] = [
    WebKey::Tunnel,
    WebKey::Stable,
    WebKey::CloudflaredName,
    WebKey::CloudflaredHost,
];

impl WebKey {
    /// The key's name under `[web]`, e.g. `cloudflared_name`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Tunnel => "tunnel",
            Self::Stable => "stable",
            Self::CloudflaredName => "cloudflared_name",
            Self::CloudflaredHost => "cloudflared_host",
        }
    }

    /// The key a `[web]` name is, or [`None`].
    pub fn parse(name: &str) -> Option<WebKey> {
        WEB_KEYS.into_iter().find(|key| key.name() == name)
    }
}

/// One settings key, as the dotted path `settings get|set|unset` takes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    /// `roles.order`: the ids a settings UI shows, in order.
    RolesOrder,
    /// `roles.<id>.<field>`.
    Role {
        /// The role's id.
        id: String,
        /// Which field of it.
        field: RoleField,
    },
    /// A `tasks.<key>`.
    Task(TaskKey),
    /// `graph.dagr`: the program that draws the run's graph, when it is not
    /// where shprd looks first.
    GraphDagr,
    /// `review.cross_vendor`: whether a reviewer must be another vendor.
    CrossVendor,
    /// `host`: which terminal host shprd drives (`auto` picks one).
    Host,
    /// A `web.<key>`: how remote access is reached.
    Web(WebKey),
}

impl Key {
    /// Parse a dotted key, accepting the names the old config file used.
    ///
    /// `land.merge`, `tick.max_running`, `tick.interval_secs`,
    /// `janitor.archive_days`, `janitor.namespace`, `review.must_fix` and
    /// `review.rounds` are the settings keys they fold into, so a person who
    /// knows the old file can ask for them by their old names.
    pub fn parse(text: &str) -> Result<Key, SettingsError> {
        let trimmed = text.trim();
        let segments: Vec<&str> = trimmed.split('.').collect();
        let unknown = || SettingsError::UnknownKey {
            key: trimmed.to_owned(),
            hint: KEY_HINT,
        };
        match segments.as_slice() {
            ["roles", "order"] => Ok(Self::RolesOrder),
            ["roles", id, field] => {
                check_role_id(id).map_err(|reason| SettingsError::BadValue {
                    key: trimmed.to_owned(),
                    reason,
                })?;
                let field = RoleField::parse(field).ok_or_else(unknown)?;
                Ok(Self::Role {
                    id: (*id).to_owned(),
                    field,
                })
            }
            ["roles", ..] => Err(SettingsError::BadValue {
                key: trimmed.to_owned(),
                reason: "names a role: write `roles.<id>.<field>`, where an id has no `.` in it"
                    .to_owned(),
            }),
            ["tasks", name] => TaskKey::parse(name).map(Self::Task).ok_or_else(unknown),
            ["graph", "dagr"] => Ok(Self::GraphDagr),
            ["review", "cross_vendor"] => Ok(Self::CrossVendor),
            ["host"] => Ok(Self::Host),
            ["web", name] => WebKey::parse(name).map(Self::Web).ok_or_else(unknown),
            ["land", "merge"] => Ok(Self::Task(TaskKey::Merge)),
            ["land", "gh_user"] => Ok(Self::Task(TaskKey::GhUser)),
            ["tick", "max_running"] => Ok(Self::Task(TaskKey::MaxRunning)),
            ["tick", "interval_secs"] => Ok(Self::Task(TaskKey::TickSecs)),
            ["janitor", "archive_days"] => Ok(Self::Task(TaskKey::ArchiveAfter)),
            ["janitor", "namespace"] => Ok(Self::Task(TaskKey::ArchiveNamespace)),
            ["review", "must_fix"] => Ok(Self::Task(TaskKey::MustFix)),
            ["review", "rounds"] => Ok(Self::Task(TaskKey::ReviewRounds)),
            _ => Err(unknown()),
        }
    }

    /// The key's canonical dotted name, e.g. `roles.worker.agent`.
    pub fn name(&self) -> String {
        match self {
            Self::RolesOrder => "roles.order".to_owned(),
            Self::Role { id, field } => format!("roles.{id}.{}", field.name()),
            Self::Task(key) => format!("tasks.{}", key.name()),
            Self::GraphDagr => "graph.dagr".to_owned(),
            Self::CrossVendor => "review.cross_vendor".to_owned(),
            Self::Host => "host".to_owned(),
            Self::Web(key) => format!("web.{}", key.name()),
        }
    }

    /// What value this key takes.
    pub fn kind(&self) -> Kind {
        match self {
            Self::RolesOrder => Kind::List,
            Self::Role { field, .. } => match field {
                RoleField::Fast | RoleField::Readonly => Kind::Switch,
                RoleField::Fallback | RoleField::Agents => Kind::List,
                RoleField::Kind => Kind::OneOf(&ROLE_KINDS),
                _ => Kind::Text,
            },
            Self::Task(key) => match key {
                TaskKey::Proof | TaskKey::Worktrees | TaskKey::SelfUpdate | TaskKey::AutoClean => {
                    Kind::Switch
                }
                TaskKey::Merge | TaskKey::MustFix => match key.words() {
                    Some(words) => Kind::OneOf(words),
                    None => Kind::Text,
                },
                TaskKey::ReviewRounds
                | TaskKey::MaxRunning
                | TaskKey::ArchiveAfter
                | TaskKey::TickSecs
                | TaskKey::NewsSettleSecs => Kind::Number,
                _ => Kind::Text,
            },
            Self::GraphDagr => Kind::Text,
            Self::CrossVendor => Kind::Switch,
            Self::Host => Kind::OneOf(&HOST_WORDS),
            Self::Web(WebKey::Tunnel) => Kind::OneOf(&WEB_TUNNEL_WORDS),
            Self::Web(WebKey::Stable) => Kind::Switch,
            Self::Web(_) => Kind::Text,
        }
    }

    /// Whether this key decides who may review, and so whether a reviewer is
    /// another vendor: a role's field, or the cross-vendor switch itself.
    ///
    /// Such a key is only stored once the harness table has been asked: storing
    /// one unchecked would let a same-vendor reviewer in with no warning.
    pub fn decides_a_review(&self) -> bool {
        matches!(self, Self::Role { .. } | Self::CrossVendor)
    }

    /// Whether a key resolves inside a repository's committed file.
    pub fn is_repo_allowed(&self) -> bool {
        match self {
            Self::Task(key) => key.is_repo_allowed(),
            _ => false,
        }
    }

    /// Check one value against this key, in the words a refusal uses.
    pub fn check(&self, value: &Value) -> Result<(), String> {
        let kind = self.kind();
        let fits = match kind {
            Kind::Text => matches!(value, Value::Text(_)),
            Kind::Number => matches!(value, Value::Number(_)),
            Kind::Switch => matches!(value, Value::Switch(_)),
            Kind::List => matches!(value, Value::List(_)),
            Kind::OneOf(words) => match value.text() {
                Some(text) => words.contains(&text),
                None => false,
            },
        };
        if !fits {
            return Err(match value {
                Value::Text(text) if matches!(kind, Kind::OneOf(_)) => {
                    format!("{}; got {text:?}", kind.expects())
                }
                _ => kind.expects(),
            });
        }
        if let Value::Number(number) = value
            && let Some((low, high)) = self.task_range()
            && (*number < low || *number > high)
        {
            return Err(format!("takes {low} to {high}, got {number}"));
        }
        if let Value::List(items) = value {
            for item in items {
                if item.trim().is_empty() {
                    return Err("takes a list with no empty item".to_owned());
                }
                if let Self::RolesOrder = self {
                    check_role_id(item)?;
                }
            }
        }
        if let Self::Role {
            field: RoleField::Prompt,
            ..
        } = self
            && let Some(text) = value.text()
        {
            // A prompt is `prompt:<id>` or a path: the same rule the role is
            // read with, checked when it is set so a broken reference is a
            // refusal now rather than a spawn that cannot read its words.
            crate::prompts::PromptRef::parse(text).map_err(|err| err.to_string())?;
        }
        Ok(())
    }

    /// The value as it is stored: a role's name is trimmed and cut to
    /// [`TITLE_MAX`] characters, and a name with nothing in it is no value at all
    /// (the role goes back to its built-in title). Every other value is kept.
    pub fn normalized(&self, value: Option<Value>) -> Option<Value> {
        let Self::Role {
            field: RoleField::Title,
            ..
        } = self
        else {
            return value;
        };
        let text = normalize_title(value?.text()?);
        (!text.is_empty()).then_some(Value::Text(text))
    }

    /// The numeric range of a `tasks.*` key, when it has one.
    fn task_range(&self) -> Option<(i64, i64)> {
        match self {
            Self::Task(key) => key.range(),
            _ => None,
        }
    }

    /// Parse the words one value was typed as on the command line.
    ///
    /// Numbers are typed as digits, switches as `true`/`false` (or `on`/`off`),
    /// lists as items separated by commas, and everything else as its own text.
    pub fn value_from_text(&self, text: &str) -> Result<Value, SettingsError> {
        let trimmed = text.trim();
        let bad = |reason: String| SettingsError::BadValue {
            key: self.name(),
            reason,
        };
        let value = match self.kind() {
            Kind::Text | Kind::OneOf(_) => Value::Text(trimmed.to_owned()),
            Kind::Number => {
                let number = trimmed
                    .parse::<i64>()
                    .map_err(|_| bad(format!("takes a whole number, got {trimmed:?}")))?;
                Value::Number(number)
            }
            Kind::Switch => {
                let on = match trimmed {
                    "true" | "on" | "yes" => true,
                    "false" | "off" | "no" => false,
                    _ => return Err(bad(format!("takes true or false, got {trimmed:?}"))),
                };
                Value::Switch(on)
            }
            Kind::List => {
                let items: Vec<String> = trimmed
                    .split(',')
                    .map(|item| item.trim().to_owned())
                    .filter(|item| !item.is_empty())
                    .collect();
                Value::List(items)
            }
        };
        self.check(&value).map_err(bad)?;
        Ok(value)
    }
}

/// Why a settings file, a key or a value was refused.
#[derive(Debug, Error)]
pub enum SettingsError {
    /// A settings file could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The file that failed.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A settings file could not be written.
    #[error("cannot write {path}: {source}")]
    Write {
        /// The file that failed.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The settings file changed between being read and being written.
    #[error("{path} changed while it was being edited; nothing was written, try again")]
    Changed {
        /// The file that moved.
        path: PathBuf,
    },
    /// A settings file holds something this build does not read.
    #[error("{path}: {source}")]
    InFile {
        /// The file the message is about.
        path: PathBuf,
        /// What is wrong with it.
        source: Box<SettingsError>,
    },
    /// The text is not valid TOML.
    #[error("is not valid TOML: {source}")]
    Toml {
        /// The underlying parse error.
        source: Box<toml::de::Error>,
    },
    /// The document cannot be read as TOML, so it cannot be edited.
    #[error("is not valid TOML: {source}")]
    Document {
        /// The underlying parse error.
        source: Box<toml_edit::TomlError>,
    },
    /// A key is not a settings key.
    #[error("unknown settings key `{key}`: {hint}")]
    UnknownKey {
        /// The key as it was written.
        key: String,
        /// What the known keys look like.
        hint: &'static str,
    },
    /// A key's value is not one it accepts.
    #[error("`{key}` {reason}")]
    BadValue {
        /// The key or field the value belongs to.
        key: String,
        /// Why the value was refused.
        reason: String,
    },
    /// A repository's committed file set a key it may not set.
    #[error("`{key}` may not be set here: {reason}")]
    NotAllowed {
        /// The key as it appeared in the file.
        key: String,
        /// Why the file may not set it.
        reason: &'static str,
    },
    /// The file declares a format this build does not read.
    #[error("declares version {found}, but this build reads version {expected}")]
    Version {
        /// The version the file declares.
        found: i64,
        /// The version this build reads.
        expected: u32,
    },
}

/// Whether a role id is one a key may name.
///
/// Any id a dotted key can hold is a role: letters of either case, digits, `-`
/// and `_`. `Docs`, `doc-writer` and `QA_2` are all ids; `order` is not, because
/// `roles.order` is the list of ids, and a `.` is not, because it is the
/// separator a key is split on.
fn check_role_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("names no role id".to_owned());
    }
    if id == "order" {
        return Err("`order` is taken: `roles.order` is the list of ids".to_owned());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "names the role {id:?} in an id a key cannot hold: an id is letters, digits, `-` or `_`, with no dots, spaces or other punctuation"
        ));
    }
    Ok(())
}

/// Where one layer of settings fills a key from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sourced<T> {
    /// The value that won.
    pub value: T,
    /// The layer it came from.
    pub source: Source,
}

impl<T> Sourced<T> {
    /// The value with the layer that set it.
    pub fn new(value: T, source: Source) -> Self {
        Self { value, source }
    }
}

/// One layer: the keys it sets and the values it sets them to.
///
/// The keys are the canonical dotted names ([`Key::name`]), so two layers can be
/// compared key by key without re-parsing anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layer {
    entries: BTreeMap<String, Value>,
}

impl Layer {
    /// An empty layer.
    pub fn new() -> Self {
        Self::default()
    }

    /// The value this layer sets `key` to, if it sets it.
    pub fn get(&self, key: &Key) -> Option<&Value> {
        self.entries.get(&key.name())
    }

    /// The value this layer sets the key named `name` to, if it sets it.
    pub fn get_named(&self, name: &str) -> Option<&Value> {
        self.entries.get(name)
    }

    /// Set one key, returning the value it replaced.
    pub fn set(&mut self, key: Key, value: Value) -> Option<Value> {
        self.entries.insert(key.name(), value)
    }

    /// Remove one key, returning the value it held.
    pub fn remove(&mut self, key: &Key) -> Option<Value> {
        self.entries.remove(&key.name())
    }

    /// Every key and value, in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.entries
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }

    /// The canonical names of the keys this layer sets.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// How many keys this layer sets.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether this layer sets nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Copy every key of `other` over this layer.
    pub fn extend(&mut self, other: &Layer) {
        for (name, value) in &other.entries {
            self.entries.insert(name.clone(), value.clone());
        }
    }

    /// The role ids this layer names, other than the order list.
    pub fn role_ids(&self) -> BTreeSet<String> {
        let mut ids = BTreeSet::new();
        for name in self.entries.keys() {
            let mut segments = name.split('.');
            if segments.next() != Some("roles") {
                continue;
            }
            if let Some(id) = segments.next()
                && id != "order"
            {
                ids.insert(id.to_owned());
            }
        }
        ids
    }
}

/// The person's settings file, split into their defaults and their per-repo entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct File {
    /// The defaults: the top level of `settings.toml`.
    pub you: Layer,
    /// The keys the old `config.toml` still sets, one layer below [`Self::you`].
    pub legacy: Layer,
    /// One entry per repository, keyed by the repository's own name.
    pub repos: BTreeMap<String, Layer>,
}

impl File {
    /// Parse the text of a `settings.toml`.
    pub fn parse(text: &str) -> Result<File, SettingsError> {
        let table: toml::Table = toml::from_str(text).map_err(|source| SettingsError::Toml {
            source: Box::new(source),
        })?;
        let mut defaults = toml::Table::new();
        let mut repos = BTreeMap::new();
        for (name, item) in table {
            match name.as_str() {
                "version" => check_version(&item)?,
                "repos" => {
                    let entries = item.as_table().ok_or_else(|| SettingsError::BadValue {
                        key: "repos".to_owned(),
                        reason: "must be a table of one entry per repository".to_owned(),
                    })?;
                    for (repo, entry) in entries {
                        if repo.trim().is_empty() {
                            return Err(SettingsError::BadValue {
                                key: "repos".to_owned(),
                                reason: "has an entry with no name".to_owned(),
                            });
                        }
                        let entry = entry.as_table().ok_or_else(|| SettingsError::BadValue {
                            key: format!("repos.{repo}"),
                            reason: "must be a table of settings keys".to_owned(),
                        })?;
                        let origin = format!("repos.{repo}.");
                        repos.insert(repo.clone(), parse_sections(entry, &origin, Limit::All)?);
                    }
                }
                _ => {
                    defaults.insert(name, item);
                }
            }
        }
        let you = parse_sections(&defaults, "", Limit::All)?;
        Ok(File {
            you,
            legacy: Layer::new(),
            repos,
        })
    }

    /// Read the person's settings, with the keys their old config file still
    /// sets applied underneath.
    ///
    /// A missing file is an empty layer: a fresh install has no settings and
    /// resolves to the built-in ones. A file that exists but cannot be read or
    /// parsed is an error, never silently dropped. The text the settings file
    /// held comes back too, because a write compares against it.
    fn read(home: &Path) -> Result<(File, Option<String>), SettingsError> {
        let mut file = File::default();
        if let Some(legacy) = read_optional(&user_config(home))? {
            let layer: ConfigLayer =
                toml::from_str(&legacy).map_err(|source| SettingsError::InFile {
                    path: user_config(home),
                    source: Box::new(SettingsError::Toml {
                        source: Box::new(source),
                    }),
                })?;
            file.legacy = layer.settings_projection();
        }
        let text = read_optional(&user_file(home))?;
        if let Some(text) = &text {
            let parsed = File::parse(text).map_err(|source| SettingsError::InFile {
                path: user_file(home),
                source: Box::new(source),
            })?;
            file.you.extend(&parsed.you);
            for (name, layer) in parsed.repos {
                file.repos.insert(name, layer);
            }
        }
        Ok((file, text))
    }
}

/// Read a repository's committed settings file.
///
/// A missing file is an empty layer; a file that exists but holds a key it may
/// not set is a refusal naming the key, so a repository cannot widen its own
/// guards by editing the file.
pub fn read_repo_file(root: &Path) -> Result<Layer, SettingsError> {
    let path = repo_file(root);
    let text = match read_optional(&path)? {
        Some(text) => text,
        None => return Ok(Layer::new()),
    };
    parse_repo_committed(&text).map_err(|source| SettingsError::InFile {
        path,
        source: Box::new(source),
    })
}

/// The prefix this repository's auto-assigned task ids carry, e.g. `SHP`.
///
/// `tasks.id_prefix` when it is set (and has a letter or digit in it), else
/// derived from the repository directory's name, so it is stable for a
/// repository. A settings file that cannot be read derives it.
pub fn task_id_prefix(home: Option<&Path>, repo: &Path) -> String {
    let set = home
        .and_then(|home| Loaded::read(home, Some(repo)).ok())
        .map(|loaded| crate::event::clean_id_prefix(&loaded.resolve(None).tasks().id_prefix))
        .unwrap_or_default();
    if !set.is_empty() {
        return set;
    }
    let name = repo
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    crate::event::derive_id_prefix(&name)
}

/// Read a file, or [`None`] when it does not exist.
fn read_optional(path: &Path) -> Result<Option<String>, SettingsError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SettingsError::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// The person's settings file: `<home>/.config/shprd/settings.toml`.
pub fn user_file(home: &Path) -> PathBuf {
    config_dir(home).join("settings.toml")
}

/// The person's pre-M9 config file: `<home>/.config/shprd/config.toml`.
pub fn user_config(home: &Path) -> PathBuf {
    config_dir(home).join("config.toml")
}

/// The directory the person's files live in.
fn config_dir(home: &Path) -> PathBuf {
    home.join(".config").join("shprd")
}

/// A repository's committed settings file: `<root>/.shprd/settings.toml`.
pub fn repo_file(root: &Path) -> PathBuf {
    root.join(".shprd").join("settings.toml")
}

/// The name a repository's entry is keyed by: the folder's own name.
///
/// Shepherdr keys a layer by the space's id; shprd has folders, so the folder's
/// name is the key. Two repositories that share a folder name share an entry.
pub fn repo_name(root: &Path) -> Option<String> {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// Which keys a table may hold: every known one, or the repository allow-list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Limit {
    /// The person's own file.
    All,
    /// The repository's committed file.
    RepoCommitted,
}

/// Check that `table`'s `version` is the one this build reads.
fn check_version(item: &toml::Value) -> Result<(), SettingsError> {
    match item.as_integer() {
        Some(found) if found == i64::from(VERSION) => Ok(()),
        Some(found) => Err(SettingsError::Version {
            found,
            expected: VERSION,
        }),
        None => Err(SettingsError::BadValue {
            key: "version".to_owned(),
            reason: "must be a whole number".to_owned(),
        }),
    }
}

/// Parse a repository's committed file, which may set only [`REPO_KEYS`].
fn parse_repo_committed(text: &str) -> Result<Layer, SettingsError> {
    let table: toml::Table = toml::from_str(text).map_err(|source| SettingsError::Toml {
        source: Box::new(source),
    })?;
    let mut tasks = toml::Table::new();
    for (name, item) in table {
        match name.as_str() {
            "version" => check_version(&item)?,
            "tasks" => {
                let section = item.as_table().ok_or_else(|| SettingsError::BadValue {
                    key: "tasks".to_owned(),
                    reason: "must be a table".to_owned(),
                })?;
                for (key, value) in section {
                    tasks.insert(key.clone(), value.clone());
                }
            }
            _ => {
                return Err(SettingsError::NotAllowed {
                    key: name,
                    reason: REPO_LIMIT,
                });
            }
        }
    }
    let mut root = toml::Table::new();
    if !tasks.is_empty() {
        root.insert("tasks".to_owned(), toml::Value::Table(tasks));
    }
    parse_sections(&root, "", Limit::RepoCommitted)
}

/// Parse the `roles`, `tasks` and `review` tables of one layer.
///
/// `origin` prefixes the key names in error messages, so a bad key inside
/// `[repos.mine.tasks]` reads as `repos.mine.tasks.merge`.
fn parse_sections(table: &toml::Table, origin: &str, limit: Limit) -> Result<Layer, SettingsError> {
    let mut layer = Layer::new();
    for (name, item) in table {
        match name.as_str() {
            "version" => check_version(item)?,
            "roles" => parse_roles(item, origin, limit, &mut layer)?,
            "tasks" => {
                let section = item.as_table().ok_or_else(|| SettingsError::BadValue {
                    key: named(origin, "tasks"),
                    reason: "must be a table".to_owned(),
                })?;
                for (field, value) in section {
                    let key =
                        TaskKey::parse(field).ok_or_else(|| unknown_key(origin, "tasks", field))?;
                    store(&mut layer, Key::Task(key), value, origin, limit)?;
                }
            }
            "graph" => {
                let section = item.as_table().ok_or_else(|| SettingsError::BadValue {
                    key: named(origin, "graph"),
                    reason: "must be a table".to_owned(),
                })?;
                for (field, value) in section {
                    match field.as_str() {
                        "dagr" => store(&mut layer, Key::GraphDagr, value, origin, limit)?,
                        other => {
                            return Err(SettingsError::UnknownKey {
                                key: named(origin, &format!("graph.{other}")),
                                hint: KEY_HINT,
                            });
                        }
                    }
                }
            }
            "review" => {
                let section = item.as_table().ok_or_else(|| SettingsError::BadValue {
                    key: named(origin, "review"),
                    reason: "must be a table".to_owned(),
                })?;
                for (field, value) in section {
                    match field.as_str() {
                        "cross_vendor" => {
                            store(&mut layer, Key::CrossVendor, value, origin, limit)?;
                        }
                        other => {
                            return Err(SettingsError::UnknownKey {
                                key: named(origin, &format!("review.{other}")),
                                hint: KEY_HINT,
                            });
                        }
                    }
                }
            }
            "host" => store(&mut layer, Key::Host, item, origin, limit)?,
            "web" => {
                let section = item.as_table().ok_or_else(|| SettingsError::BadValue {
                    key: named(origin, "web"),
                    reason: "must be a table".to_owned(),
                })?;
                for (field, value) in section {
                    let key =
                        WebKey::parse(field).ok_or_else(|| unknown_key(origin, "web", field))?;
                    store(&mut layer, Key::Web(key), value, origin, limit)?;
                }
            }
            other => {
                return Err(SettingsError::UnknownKey {
                    key: named(origin, other),
                    hint: KEY_HINT,
                });
            }
        }
    }
    Ok(layer)
}

/// The refusal an unknown name inside a section produces.
fn unknown_key(origin: &str, section: &str, name: &str) -> SettingsError {
    SettingsError::UnknownKey {
        key: named(origin, &format!("{section}.{name}")),
        hint: KEY_HINT,
    }
}

/// Parse one `[roles]` table: the order list and one table per role.
fn parse_roles(
    item: &toml::Value,
    origin: &str,
    limit: Limit,
    layer: &mut Layer,
) -> Result<(), SettingsError> {
    let section = item.as_table().ok_or_else(|| SettingsError::BadValue {
        key: named(origin, "roles"),
        reason: "must be a table of roles keyed by id".to_owned(),
    })?;
    for (name, value) in section {
        if name == "order" {
            store(layer, Key::RolesOrder, value, origin, limit)?;
            continue;
        }
        check_role_id(name).map_err(|reason| SettingsError::BadValue {
            key: named(origin, "roles.<id>"),
            reason,
        })?;
        let fields = value.as_table().ok_or_else(|| SettingsError::BadValue {
            key: named(origin, &format!("roles.{name}")),
            reason: "must be a table of the role's fields".to_owned(),
        })?;
        for (field, value) in fields {
            let field = RoleField::parse(field).ok_or_else(|| SettingsError::UnknownKey {
                key: named(origin, &format!("roles.{name}.{field}")),
                hint: KEY_HINT,
            })?;
            store(
                layer,
                Key::Role {
                    id: name.clone(),
                    field,
                },
                value,
                origin,
                limit,
            )?;
        }
    }
    Ok(())
}

/// Convert and check one TOML value, then put it in the layer.
fn store(
    layer: &mut Layer,
    key: Key,
    item: &toml::Value,
    origin: &str,
    limit: Limit,
) -> Result<(), SettingsError> {
    if limit == Limit::RepoCommitted && !key.is_repo_allowed() {
        return Err(SettingsError::NotAllowed {
            key: named(origin, &key.name()),
            reason: REPO_LIMIT,
        });
    }
    let value = value_of(item).ok_or_else(|| SettingsError::BadValue {
        key: named(origin, &key.name()),
        reason: key.kind().expects(),
    })?;
    key.check(&value)
        .map_err(|reason| SettingsError::BadValue {
            key: named(origin, &key.name()),
            reason,
        })?;
    layer.set(key, value);
    Ok(())
}

/// The value a TOML item holds, when it is one settings carry.
fn value_of(item: &toml::Value) -> Option<Value> {
    match item {
        toml::Value::String(text) => Some(Value::Text(text.clone())),
        toml::Value::Integer(number) => Some(Value::Number(*number)),
        toml::Value::Boolean(on) => Some(Value::Switch(*on)),
        toml::Value::Array(items) => {
            let mut words = Vec::with_capacity(items.len());
            for item in items {
                words.push(item.as_str()?.to_owned());
            }
            Some(Value::List(words))
        }
        _ => None,
    }
}

/// A key name, prefixed with the table it sits in when that is not the top one.
fn named(origin: &str, key: &str) -> String {
    if origin.is_empty() {
        key.to_owned()
    } else {
        format!("{origin}{key}")
    }
}

/// The values this build ships, as one layer.
///
/// The workflow values come from [`Config::default`], so a person who has never
/// written a settings file resolves to exactly the values the old config file
/// defaulted to. Roles ship with their identity (title, about, kind, prompt) and
/// with no pick at all: which program a role runs is the person's choice, and an
/// unset pick leaves it to the harness's own defaults.
pub fn builtin() -> Layer {
    let config = Config::default();
    let mut layer = Layer::new();
    layer.set(
        Key::Task(TaskKey::Merge),
        Value::Text(merge_name(config.land.merge).to_owned()),
    );
    layer.set(
        Key::Task(TaskKey::ReviewRounds),
        Value::Number(i64::from(config.review.rounds)),
    );
    layer.set(
        Key::Task(TaskKey::MustFix),
        Value::Text(severity_name(config.review.must_fix).to_owned()),
    );
    layer.set(Key::Task(TaskKey::Proof), Value::Switch(true));
    layer.set(
        Key::Task(TaskKey::MaxRunning),
        Value::Number(i64::from(config.tick.max_running)),
    );
    layer.set(
        Key::Task(TaskKey::ArchiveAfter),
        Value::Number(i64::from(config.janitor.archive_days)),
    );
    layer.set(
        Key::Task(TaskKey::ArchiveNamespace),
        Value::Text(config.janitor.namespace),
    );
    layer.set(Key::Task(TaskKey::Worktrees), Value::Switch(true));
    layer.set(Key::Task(TaskKey::SelfUpdate), Value::Switch(false));
    layer.set(Key::Task(TaskKey::AutoClean), Value::Switch(true));
    layer.set(Key::Task(TaskKey::Base), Value::Text(String::new()));
    layer.set(Key::Task(TaskKey::GhUser), Value::Text(String::new()));
    layer.set(Key::Task(TaskKey::IdPrefix), Value::Text(String::new()));
    layer.set(
        Key::Task(TaskKey::Branch),
        Value::Text(DEFAULT_BRANCH.to_owned()),
    );
    layer.set(Key::Task(TaskKey::StartNotes), Value::Text(String::new()));
    layer.set(Key::Task(TaskKey::FinishNotes), Value::Text(String::new()));
    layer.set(
        Key::Task(TaskKey::WorktreeFiles),
        Value::Text(String::new()),
    );
    layer.set(
        Key::Task(TaskKey::TickSecs),
        Value::Number(i64::try_from(config.tick.interval_secs).unwrap_or(15)),
    );
    layer.set(
        Key::Task(TaskKey::NewsSettleSecs),
        Value::Number(i64::try_from(NEWS_SETTLE_SECS).unwrap_or(5)),
    );
    layer.set(Key::GraphDagr, Value::Text(String::new()));
    layer.set(Key::CrossVendor, Value::Switch(true));
    layer.set(Key::Host, Value::Text(HostChoice::Auto.name().to_owned()));
    layer.set(
        Key::Web(WebKey::Tunnel),
        Value::Text(WEB_TUNNEL_WORDS[0].to_owned()),
    );
    layer.set(Key::Web(WebKey::Stable), Value::Switch(false));
    layer.set(
        Key::Web(WebKey::CloudflaredName),
        Value::Text(String::new()),
    );
    layer.set(
        Key::Web(WebKey::CloudflaredHost),
        Value::Text(String::new()),
    );
    layer.set(
        Key::RolesOrder,
        Value::List(BUILTIN_ROLES.iter().map(|id| (*id).to_owned()).collect()),
    );
    for (id, role) in builtin_roles() {
        for field in ROLE_FIELDS {
            if let Some(value) = role.get(field) {
                layer.set(
                    Key::Role {
                        id: id.clone(),
                        field,
                    },
                    value,
                );
            }
        }
    }
    layer
}

/// The branch pattern new tasks are named after, when nobody set one: what
/// follows the archive namespace. `{run}`, `{task}`, `{n}` and `{slug}` stand
/// for the run id, the task id, its number and its lowercased id.
pub const DEFAULT_BRANCH: &str = "{run}/{task}";

/// How long a change must hold before a space's coordinator is told about it,
/// in seconds: Shepherdr's settle window (`news.py`'s `NEWS_SETTLE_MS`), used
/// when nobody sets `tasks.news_settle_secs`.
pub const NEWS_SETTLE_SECS: u64 = 5;

/// The identity of the four roles this build ships.
///
/// Only the identity is here: a pick belongs to the person who runs it.
fn builtin_roles() -> Vec<(String, RoleDefaults)> {
    vec![
        (
            "coordinator".to_owned(),
            RoleDefaults {
                title: "Assistant".to_owned(),
                about: "Plans the work and starts a task for each part.".to_owned(),
                kind: RoleKind::Coordinator,
                prompt: "prompt:assistant".to_owned(),
                readonly: false,
            },
        ),
        (
            "worker".to_owned(),
            RoleDefaults {
                title: "Worker".to_owned(),
                about: "Does the work of one task, on its own branch.".to_owned(),
                kind: RoleKind::Worker,
                prompt: "prompt:worker".to_owned(),
                readonly: false,
            },
        ),
        (
            "reviewer".to_owned(),
            RoleDefaults {
                title: "Reviewer".to_owned(),
                about: "Reads a finished attempt's diff and decides.".to_owned(),
                kind: RoleKind::Reviewer,
                prompt: "prompt:reviewer".to_owned(),
                readonly: true,
            },
        ),
    ]
}

/// The identity of a shipped role.
struct RoleDefaults {
    /// The word the person sees.
    title: String,
    /// One line about what the role does.
    about: String,
    /// What the role is for.
    kind: RoleKind,
    /// The instructions the role starts from.
    prompt: String,
    /// Whether it only reads.
    readonly: bool,
}

impl RoleDefaults {
    /// The value this role gives one of its fields, if it is an identity field.
    fn get(&self, field: RoleField) -> Option<Value> {
        match field {
            RoleField::Title => Some(Value::Text(self.title.clone())),
            RoleField::About => Some(Value::Text(self.about.clone())),
            RoleField::Kind => Some(Value::Text(self.kind.name().to_owned())),
            RoleField::Readonly => Some(Value::Switch(self.readonly)),
            RoleField::Fast => Some(Value::Switch(false)),
            RoleField::Fallback | RoleField::Agents => Some(Value::List(Vec::new())),
            RoleField::Prompt => Some(Value::Text(self.prompt.clone())),
            _ => None,
        }
    }
}

/// The words a merge policy is stored as.
pub fn merge_name(policy: MergePolicy) -> &'static str {
    match policy {
        MergePolicy::Ask => "ask",
        MergePolicy::Checks => "checks",
        MergePolicy::Auto => "auto",
    }
}

/// The policy a word names, if it names one.
pub fn merge_of(text: &str) -> Option<MergePolicy> {
    match text {
        "ask" => Some(MergePolicy::Ask),
        "checks" => Some(MergePolicy::Checks),
        "auto" | "allow" => Some(MergePolicy::Auto),
        _ => None,
    }
}

/// The words a severity is stored as.
pub fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Critical => "critical",
        Severity::Major => "major",
        Severity::Minor => "minor",
        Severity::Trivial => "trivial",
    }
}

/// The severity a word names, if it names one.
pub fn severity_of(text: &str) -> Option<Severity> {
    match text {
        "critical" => Some(Severity::Critical),
        "major" => Some(Severity::Major),
        "minor" => Some(Severity::Minor),
        "trivial" => Some(Severity::Trivial),
        _ => None,
    }
}

/// What one resolution is asked.
#[derive(Debug, Clone, Copy, Default)]
pub struct Request<'a> {
    /// The person's entry for this repository, by name.
    pub repo: Option<&'a str>,
    /// The repository's committed layer, when it has one.
    pub repo_file: Option<&'a Layer>,
    /// The task's own overrides, when the caller has any.
    pub task: Option<&'a Layer>,
}

/// Resolve every key, nearest layer first, and say where each value came from.
///
/// The chain is the task's own record, the person's entry for this repository,
/// the person's defaults, the keys their old `config.toml` still sets, the
/// repository's committed file and then the built-in values. A role named in any
/// layer, or one of the built-ins, is reported with every field: a field nobody
/// set is reported unset, and a custom role gets its built-in identity (its id
/// as its title, `helper` as its kind, its own prompt file).
pub fn resolve(file: &File, request: &Request<'_>) -> Resolved {
    let builtin = builtin();
    let repo_entry = request.repo.and_then(|name| file.repos.get(name));
    let chain: [(Option<&Layer>, Source); 6] = [
        (request.task, Source::Task),
        (repo_entry, Source::RepoEntry),
        (Some(&file.you), Source::You),
        (Some(&file.legacy), Source::Legacy),
        (request.repo_file, Source::RepoFile),
        (Some(&builtin), Source::BuiltIn),
    ];

    let mut names: BTreeSet<&str> = BTreeSet::new();
    for (layer, _) in &chain {
        if let Some(layer) = layer {
            names.extend(layer.keys());
        }
    }
    let mut values: BTreeMap<String, Sourced<Value>> = BTreeMap::new();
    for name in names {
        for (layer, source) in &chain {
            if let Some(value) = layer.and_then(|layer| layer.get_named(name)) {
                values.insert(name.to_owned(), Sourced::new(value.clone(), *source));
                break;
            }
        }
    }

    // A role exists when a layer names one of its fields or it is built in.
    let mut ids: BTreeSet<String> = BUILTIN_ROLES.iter().map(|id| (*id).to_owned()).collect();
    for (layer, _) in &chain {
        if let Some(layer) = layer {
            ids.extend(layer.role_ids());
        }
    }
    ids.retain(|id| !RETIRED_ROLES.contains(&id.as_str()));
    values.retain(|name, _| {
        !RETIRED_ROLES
            .iter()
            .any(|id| name.starts_with(&format!("roles.{id}.")))
    });
    if let Some(Sourced { value, .. }) = values.get("roles.order")
        && let Some(order) = value.list()
    {
        for id in order {
            if !RETIRED_ROLES.contains(&id.as_str()) {
                ids.insert(id.clone());
            }
        }
    }
    for id in &ids {
        for field in ROLE_FIELDS {
            let key = Key::Role {
                id: id.clone(),
                field,
            };
            if let Some(value) = custom_default(id).get(field) {
                values
                    .entry(key.name())
                    .or_insert(Sourced::new(value, Source::BuiltIn));
            }
        }
    }

    let order = role_order(&values, &ids);
    Resolved {
        values,
        roles: order,
    }
}

/// The identity a custom role gets for the fields nobody set.
fn custom_default(id: &str) -> RoleDefaults {
    RoleDefaults {
        title: id.to_owned(),
        about: String::new(),
        kind: RoleKind::Helper,
        prompt: format!("prompt:{id}"),
        readonly: false,
    }
}

/// The order a settings UI shows the roles in.
///
/// The explicit `roles.order` first, then the built-ins it left out, then any
/// other role by id, so a role a person added never disappears from the list.
fn role_order(values: &BTreeMap<String, Sourced<Value>>, ids: &BTreeSet<String>) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    if let Some(explicit) = values.get("roles.order").and_then(|d| d.value.list()) {
        for id in explicit {
            if ids.contains(id) && !order.contains(id) {
                order.push(id.clone());
            }
        }
    }
    for id in BUILTIN_ROLES {
        if !order.iter().any(|seen| seen == id) {
            order.push(id.to_owned());
        }
    }
    for id in ids {
        if !order.contains(id) {
            order.push(id.clone());
        }
    }
    order
}

/// One resolved role: every field, with the layer each came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRole {
    /// The role's id.
    pub id: String,
    /// The word the person sees.
    pub title: Sourced<String>,
    /// One line about what the role does.
    pub about: Sourced<String>,
    /// Who may dispatch or review as this role.
    pub kind: Sourced<RoleKind>,
    /// The instructions: `prompt:<id>` or a path.
    pub prompt: Sourced<String>,
    /// The program, when one is named.
    pub agent: Option<Sourced<String>>,
    /// The model, in the program's own words.
    pub model: Option<Sourced<String>>,
    /// The effort level, in the program's own words.
    pub effort: Option<Sourced<String>>,
    /// Whether the program's fast mode is on.
    pub fast: Sourced<bool>,
    /// The access mode, as one of the program's ids.
    pub perm: Option<Sourced<String>>,
    /// The worker lines after the first, as stored (see [`WorkerLine`]).
    pub agents: Sourced<Vec<String>>,
    /// The programs to fall back through, in order.
    pub fallback: Sourced<Vec<String>>,
    /// Whether the role only reads.
    pub readonly: Sourced<bool>,
}

/// One program line of a role that may run several: a worker's.
///
/// The first line is the role's own `agent`, `model`, `effort`, `fast` and `perm`;
/// the rest are stored in `agents` as `agent|model|effort|fast|perm` strings (an
/// empty part is unset, `fast` is `1` when on). A model id may hold any character
/// but `|`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLine {
    /// The program, as a harness id.
    pub agent: String,
    /// The model, in the program's own words.
    pub model: Option<String>,
    /// The effort level.
    pub effort: Option<String>,
    /// Whether the program's fast mode is on.
    pub fast: bool,
    /// The access mode.
    pub perm: Option<String>,
}

impl WorkerLine {
    /// A line that names only its program.
    pub fn program(agent: &str) -> WorkerLine {
        WorkerLine {
            agent: agent.to_owned(),
            model: None,
            effort: None,
            fast: false,
            perm: None,
        }
    }

    /// The line a stored string names, or [`None`] when it names no program.
    ///
    /// A bare `program` and `program/model` (the old fallback spelling) read too.
    pub fn parse(text: &str) -> Option<WorkerLine> {
        let mut parts = text.split('|');
        let head = parts.next()?.trim();
        let rest: Vec<&str> = parts.collect();
        let some = |part: Option<&&str>| {
            part.map(|part| part.trim())
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
        };
        if rest.is_empty() {
            let (agent, model) = match head.split_once('/') {
                Some((agent, model)) => (agent.trim(), Some(model.trim().to_owned())),
                None => (head, None),
            };
            if agent.is_empty() {
                return None;
            }
            return Some(WorkerLine {
                model: model.filter(|model| !model.is_empty()),
                ..WorkerLine::program(agent)
            });
        }
        if head.is_empty() {
            return None;
        }
        Some(WorkerLine {
            agent: head.to_owned(),
            model: some(rest.first()),
            effort: some(rest.get(1)),
            fast: rest
                .get(2)
                .is_some_and(|part| matches!(part.trim(), "1" | "true")),
            perm: some(rest.get(3)),
        })
    }

    /// The string this line is stored as.
    pub fn encode(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.agent,
            self.model.as_deref().unwrap_or(""),
            self.effort.as_deref().unwrap_or(""),
            if self.fast { "1" } else { "" },
            self.perm.as_deref().unwrap_or("")
        )
    }

    /// The `program` or `program/model` string a worker list names this line by.
    pub fn agent_ref(&self) -> String {
        match &self.model {
            Some(model) => format!("{}/{model}", self.agent),
            None => self.agent.clone(),
        }
    }
}

impl ResolvedRole {
    /// The role's program lines, first (the default) first.
    ///
    /// The first line is the role's own `agent` (none when no program is named);
    /// the rest are `agents`, or — for a settings file written before a role could
    /// hold several — the `fallback` programs, each as a line with the program's
    /// own defaults. A program is listed once: a later line naming it again is
    /// ignored, because a task picks a line by its program.
    pub fn lines(&self) -> Vec<WorkerLine> {
        let mut lines: Vec<WorkerLine> = Vec::new();
        // The role's own `agent` is always the first line, whatever layer set it:
        // `agents` only lists what follows when it cannot run.
        if let Some(agent) = self.agent.as_ref() {
            let some = |field: &Option<Sourced<String>>| {
                field
                    .as_ref()
                    .map(|sourced| sourced.value.trim().to_owned())
                    .filter(|text| !text.is_empty())
            };
            lines.push(WorkerLine {
                agent: agent.value.clone(),
                model: some(&self.model),
                effort: some(&self.effort),
                fast: self.fast.value,
                perm: some(&self.perm),
            });
        }
        let more = if self.agents.value.is_empty() {
            &self.fallback.value
        } else {
            &self.agents.value
        };
        for text in more {
            if let Some(line) = WorkerLine::parse(text)
                && !lines.iter().any(|kept| kept.agent == line.agent)
            {
                lines.push(line);
            }
        }
        lines
    }

    /// Whether this role may review: it is a reviewer, or it only reads.
    pub fn may_review(&self) -> bool {
        self.kind.value == RoleKind::Reviewer || self.readonly.value
    }

    /// The role's title as a settings line prints it.
    pub fn label(&self) -> &str {
        &self.title.value
    }

    /// The name the person sees for this role: its title, trimmed and cut to
    /// [`TITLE_MAX`] characters, or the role's built-in title when the stored one
    /// is empty. See [`display_title`].
    pub fn display_title(&self) -> String {
        display_title(&self.title.value, &self.id)
    }
}

/// The longest name a role may have, in characters.
///
/// A tab, a toast and a sidebar line all carry it, so it has to fit them.
pub const TITLE_MAX: usize = 24;

/// The name a role is shown under: `raw` trimmed and cut to [`TITLE_MAX`]
/// characters, or — when nothing is left — the built-in title of the role `id`
/// (a shipped role's own word, else the id itself).
pub fn display_title(raw: &str, id: &str) -> String {
    let named = normalize_title(raw);
    if !named.is_empty() {
        return named;
    }
    builtin_roles()
        .into_iter()
        .find(|(known, _)| known == id)
        .map_or_else(|| id.to_owned(), |(_, defaults)| defaults.title)
}

/// A typed role name as it is stored: trimmed, whitespace runs inside it kept
/// as one space, and cut to [`TITLE_MAX`] characters. Empty means "no name".
pub fn normalize_title(raw: &str) -> String {
    let words: Vec<&str> = raw.split_whitespace().collect();
    words
        .join(" ")
        .chars()
        .take(TITLE_MAX)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// The resolved workflow values, typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tasks {
    /// Who merges a ready task.
    pub merge: MergePolicy,
    /// Failed review rounds allowed before a human is asked.
    pub review_rounds: u32,
    /// The least severe finding a worker must fix.
    pub must_fix: Severity,
    /// Whether every task leaves one proof artifact.
    pub proof: bool,
    /// How many tasks of one repository run at once.
    pub max_running: u32,
    /// Days after which a done task archives itself; `0` never.
    pub archive_after: u32,
    /// The namespace archived resources are put under.
    pub archive_namespace: String,
    /// Whether every task gets its own worktree.
    pub worktrees: bool,
    /// Whether shprd rebuilds and restarts itself after a landed task changes it.
    pub self_update: bool,
    /// Whether the engine removes a landed or dropped task's worktree and branch itself.
    pub auto_clean: bool,
    /// The branch new tasks start from and merge into.
    pub base: String,
    /// The `gh` account pinned for forge calls; empty means the active one.
    pub gh_user: String,
    /// The prefix of auto-assigned task ids (e.g. `SHP`); empty derives it from the repository's name.
    pub id_prefix: String,
    /// The pattern new tasks' branches are named after.
    pub branch: String,
    /// Words put into a task's brief when it starts.
    pub start_notes: String,
    /// Words put into a task's brief before it reports.
    pub finish_notes: String,
    /// The files to copy or link from the main checkout into every worktree:
    /// one per line or comma-separated, `link:` before a path links it.
    pub worktree_files: String,
    /// Seconds between two passes of the service's tick.
    pub tick_secs: u64,
    /// Seconds a change must hold before a space's coordinator is told about
    /// it; `0` tells it at once.
    pub news_settle_secs: u64,
}

/// One line of `settings show`: a key, its value and the layer it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The key's canonical name, e.g. `roles.worker.agent`.
    pub key: String,
    /// The value, or [`None`] when nothing sets it.
    pub value: Option<Value>,
    /// The layer that set it, or [`None`] when nothing does.
    pub source: Option<Source>,
}

/// Every setting, resolved, with where each value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    values: BTreeMap<String, Sourced<Value>>,
    roles: Vec<String>,
}

impl Resolved {
    /// The value one key resolves to, with its layer, or [`None`].
    pub fn get(&self, key: &Key) -> Option<&Sourced<Value>> {
        self.values.get(&key.name())
    }

    /// The role ids, in the order a settings UI shows them.
    pub fn order(&self) -> &[String] {
        &self.roles
    }

    /// What each role is called, by role id, in the order a settings UI shows
    /// them: every name is [`ResolvedRole::display_title`].
    pub fn role_titles(&self) -> Vec<(String, String)> {
        self.roles()
            .into_iter()
            .map(|role| {
                let title = role.display_title();
                (role.id, title)
            })
            .collect()
    }

    /// Every role, in the order a settings UI shows them.
    pub fn roles(&self) -> Vec<ResolvedRole> {
        self.roles.iter().filter_map(|id| self.role(id)).collect()
    }

    /// One role by id, resolved.
    pub fn role(&self, id: &str) -> Option<ResolvedRole> {
        if !self.roles.iter().any(|known| known == id) {
            return None;
        }
        let text = |field: RoleField| -> Sourced<String> {
            let key = Key::Role {
                id: id.to_owned(),
                field,
            };
            let sourced = self.values.get(&key.name());
            Sourced::new(
                sourced
                    .and_then(|sourced| sourced.value.text())
                    .unwrap_or_default()
                    .to_owned(),
                sourced.map_or(Source::BuiltIn, |sourced| sourced.source),
            )
        };
        let optional = |field: RoleField| -> Option<Sourced<String>> {
            let key = Key::Role {
                id: id.to_owned(),
                field,
            };
            self.values.get(&key.name()).and_then(|sourced| {
                sourced
                    .value
                    .text()
                    .map(|text| Sourced::new(text.to_owned(), sourced.source))
            })
        };
        let kind = text(RoleField::Kind);
        Some(ResolvedRole {
            id: id.to_owned(),
            title: text(RoleField::Title),
            about: text(RoleField::About),
            kind: Sourced::new(
                RoleKind::parse(&kind.value).unwrap_or(RoleKind::Helper),
                kind.source,
            ),
            prompt: text(RoleField::Prompt),
            agent: optional(RoleField::Agent),
            model: optional(RoleField::Model),
            effort: optional(RoleField::Effort),
            fast: self.switch_of(id, RoleField::Fast),
            perm: optional(RoleField::Perm),
            agents: self.list_of(id, RoleField::Agents),
            fallback: self.list_of(id, RoleField::Fallback),
            readonly: self.switch_of(id, RoleField::Readonly),
        })
    }

    /// One role's boolean field.
    fn switch_of(&self, id: &str, field: RoleField) -> Sourced<bool> {
        let key = Key::Role {
            id: id.to_owned(),
            field,
        };
        self.values
            .get(&key.name())
            .map_or(Sourced::new(false, Source::BuiltIn), |sourced| {
                Sourced::new(sourced.value.switch().unwrap_or(false), sourced.source)
            })
    }

    /// One role's list field.
    fn list_of(&self, id: &str, field: RoleField) -> Sourced<Vec<String>> {
        let key = Key::Role {
            id: id.to_owned(),
            field,
        };
        self.values
            .get(&key.name())
            .map_or(Sourced::new(Vec::new(), Source::BuiltIn), |sourced| {
                Sourced::new(
                    sourced.value.list().unwrap_or_default().to_vec(),
                    sourced.source,
                )
            })
    }

    /// The workflow values, typed.
    pub fn tasks(&self) -> Tasks {
        let defaults = Config::default();
        Tasks {
            merge: self
                .text(TaskKey::Merge)
                .and_then(merge_of)
                .unwrap_or(defaults.land.merge),
            review_rounds: self.count(TaskKey::ReviewRounds, defaults.review.rounds),
            must_fix: self
                .text(TaskKey::MustFix)
                .and_then(severity_of)
                .unwrap_or(defaults.review.must_fix),
            proof: self.on(TaskKey::Proof, true),
            max_running: self.count(TaskKey::MaxRunning, defaults.tick.max_running),
            archive_after: self.count(TaskKey::ArchiveAfter, defaults.janitor.archive_days),
            archive_namespace: self
                .text(TaskKey::ArchiveNamespace)
                .unwrap_or(&defaults.janitor.namespace)
                .to_owned(),
            worktrees: self.on(TaskKey::Worktrees, true),
            self_update: self.on(TaskKey::SelfUpdate, false),
            auto_clean: self.on(TaskKey::AutoClean, true),
            base: self.text(TaskKey::Base).unwrap_or_default().to_owned(),
            gh_user: self
                .text(TaskKey::GhUser)
                .unwrap_or_default()
                .trim()
                .to_owned(),
            id_prefix: self
                .text(TaskKey::IdPrefix)
                .unwrap_or_default()
                .trim()
                .to_owned(),
            branch: self
                .text(TaskKey::Branch)
                .unwrap_or(DEFAULT_BRANCH)
                .to_owned(),
            start_notes: self
                .text(TaskKey::StartNotes)
                .unwrap_or_default()
                .to_owned(),
            finish_notes: self
                .text(TaskKey::FinishNotes)
                .unwrap_or_default()
                .to_owned(),
            worktree_files: self
                .text(TaskKey::WorktreeFiles)
                .unwrap_or_default()
                .to_owned(),
            tick_secs: u64::from(self.count(
                TaskKey::TickSecs,
                u32::try_from(defaults.tick.interval_secs).unwrap_or(15),
            )),
            news_settle_secs: u64::from(self.count(
                TaskKey::NewsSettleSecs,
                u32::try_from(NEWS_SETTLE_SECS).unwrap_or(5),
            )),
        }
    }

    /// The program `graph.dagr` names, or [`None`] when the graph block should
    /// find dagr itself (`PATH`, then the herdr plugin folder).
    ///
    /// An empty value is unset, which is what a person who has just reset the
    /// key expects: the blocks fall back to looking for `dagr` on its own.
    pub fn graph_dagr(&self) -> Option<&str> {
        self.get(&Key::GraphDagr)
            .and_then(|sourced| sourced.value.text())
            .map(str::trim)
            .filter(|text| !text.is_empty())
    }

    /// Which terminal host shprd drives, as the setting names it.
    ///
    /// A value this build does not know resolves to [`HostChoice::Auto`]: a
    /// settings file written by a newer shprd must not make every verb refuse,
    /// and `auto` is the choice that asks the machine what it has.
    pub fn host(&self) -> HostChoice {
        self.get(&Key::Host)
            .and_then(|sourced| sourced.value.text())
            .and_then(HostChoice::parse)
            .unwrap_or_default()
    }

    /// A `web.*` value's text, trimmed.
    fn web_text(&self, key: WebKey) -> &str {
        self.get(&Key::Web(key))
            .and_then(|sourced| sourced.value.text())
            .map(str::trim)
            .unwrap_or_default()
    }

    /// The tunnel provider `web up` uses when none is named: one of
    /// [`WEB_TUNNEL_WORDS`], `tailscale` when the value is not one.
    pub fn web_tunnel(&self) -> &str {
        let word = self.web_text(WebKey::Tunnel);
        WEB_TUNNEL_WORDS
            .into_iter()
            .find(|candidate| *candidate == word)
            .unwrap_or(WEB_TUNNEL_WORDS[0])
    }

    /// Whether tunnl.gg's stable address is asked for.
    pub fn web_stable(&self) -> bool {
        self.get(&Key::Web(WebKey::Stable))
            .and_then(|sourced| sourced.value.switch())
            .unwrap_or(false)
    }

    /// The named cloudflared tunnel's name; empty when none is set.
    pub fn web_cloudflared_name(&self) -> &str {
        self.web_text(WebKey::CloudflaredName)
    }

    /// The hostname routed to the named cloudflared tunnel; empty when none is
    /// set.
    pub fn web_cloudflared_host(&self) -> &str {
        self.web_text(WebKey::CloudflaredHost)
    }

    /// Whether a reviewer must be another vendor than the worker.
    pub fn cross_vendor(&self) -> bool {
        self.get(&Key::CrossVendor)
            .and_then(|sourced| sourced.value.switch())
            .unwrap_or(true)
    }

    /// One `tasks.*` value's text.
    fn text(&self, key: TaskKey) -> Option<&str> {
        self.get(&Key::Task(key))
            .and_then(|sourced| sourced.value.text())
    }

    /// One `tasks.*` value as a count.
    fn count(&self, key: TaskKey, default: u32) -> u32 {
        self.get(&Key::Task(key))
            .and_then(|sourced| sourced.value.number())
            .and_then(|number| u32::try_from(number).ok())
            .unwrap_or(default)
    }

    /// One `tasks.*` value as a switch.
    fn on(&self, key: TaskKey, default: bool) -> bool {
        self.get(&Key::Task(key))
            .and_then(|sourced| sourced.value.switch())
            .unwrap_or(default)
    }

    /// Every key, with its value and layer, in the order a UI lists them.
    ///
    /// Every key of the schema is here, including the ones nothing sets: a
    /// settings page has to draw an unset field too.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        self.push_row(&mut rows, Key::RolesOrder);
        for id in &self.roles {
            for field in ROLE_FIELDS {
                self.push_row(
                    &mut rows,
                    Key::Role {
                        id: id.clone(),
                        field,
                    },
                );
            }
        }
        for key in TASK_KEYS {
            self.push_row(&mut rows, Key::Task(key));
        }
        self.push_row(&mut rows, Key::GraphDagr);
        self.push_row(&mut rows, Key::CrossVendor);
        self.push_row(&mut rows, Key::Host);
        for key in WEB_KEYS {
            self.push_row(&mut rows, Key::Web(key));
        }
        rows
    }

    /// Add one key's line.
    fn push_row(&self, rows: &mut Vec<Row>, key: Key) {
        let sourced = self.get(&key);
        rows.push(Row {
            key: key.name(),
            value: sourced.map(|sourced| sourced.value.clone()),
            source: sourced.map(|sourced| sourced.source),
        });
    }

    /// Whether any resolved role names a program, so a harness table has
    /// something to check.
    pub fn names_a_pick(&self) -> bool {
        self.roles()
            .iter()
            .any(|role| role.agent.is_some() || !role.fallback.value.is_empty())
    }

    /// Whether any layer sets one key, with the layer that wins.
    pub fn source_of(&self, key: &Key) -> Option<Source> {
        self.get(key).map(|sourced| sourced.source)
    }
}

/// Everything one resolution reads from disk: the person's file and the repo's.
///
/// [`Loaded::default`] is "no files at all": nothing sets anything, so every
/// key resolves to the built-in value and every role to its shipped identity. A
/// test that does not care about settings starts there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Loaded {
    /// The person's file.
    pub file: File,
    /// The name this repository's entry is keyed by.
    pub repo: Option<String>,
    /// The repository's committed layer.
    pub repo_file: Layer,
    /// The text the person's settings file held when it was read.
    ///
    /// A write compares against it, so a file that changed in between — another
    /// verb, the service, a person's editor — is refused instead of replaced.
    settings_text: Option<String>,
}

impl Loaded {
    /// Read the person's file and `root`'s committed file.
    pub fn read(home: &Path, root: Option<&Path>) -> Result<Loaded, SettingsError> {
        let (file, settings_text) = File::read(home)?;
        let (repo, repo_file) = match root {
            Some(root) => (repo_name(root), read_repo_file(root)?),
            None => (None, Layer::new()),
        };
        Ok(Loaded {
            file,
            repo,
            repo_file,
            settings_text,
        })
    }

    /// Resolve the settings, with a task's own overrides when the caller has any.
    pub fn resolve(&self, task: Option<&Layer>) -> Resolved {
        resolve(
            &self.file,
            &Request {
                repo: self.repo.as_deref(),
                repo_file: Some(&self.repo_file),
                task,
            },
        )
    }

    /// Resolve as if `change` had already been applied to one layer.
    ///
    /// The caller checks the answer — a role that names no program, a review
    /// that is not cross-vendor — before anything is written, so a refused
    /// change never reaches the disk. [`None`] removes the key.
    pub fn preview(
        &self,
        scope: &Scope,
        key: &Key,
        change: Option<&Value>,
        task: Option<&Layer>,
    ) -> Resolved {
        let mut file = self.file.clone();
        match scope {
            Scope::You => apply(&mut file.you, key, change),
            Scope::Repo(name) => {
                let entry = file.repos.entry(name.clone()).or_default();
                apply(entry, key, change);
            }
        }
        resolve(
            &file,
            &Request {
                repo: self.repo.as_deref(),
                repo_file: Some(&self.repo_file),
                task,
            },
        )
    }

    /// Set or remove one key in `scope`, the way `shprd settings` does.
    ///
    /// A value equal to the one this scope would inherit anyway is not stored:
    /// the key is removed instead, which is what makes "reset to inherited" work
    /// with the same verb. Returns what happened and the settings as they now
    /// resolve.
    pub fn write(
        &self,
        home: &Path,
        scope: &Scope,
        key: &Key,
        change: Option<Value>,
        task: Option<&Layer>,
    ) -> Result<Written, SettingsError> {
        let current: Option<Value> = match scope {
            Scope::You => self.file.you.get(key),
            Scope::Repo(name) => self.file.repos.get(name).and_then(|layer| layer.get(key)),
        }
        .cloned();
        let (outcome, decided) = match change {
            None => match current {
                Some(_) => (WriteOutcome::Removed, None),
                None => (WriteOutcome::WasUnset, None),
            },
            Some(value) => {
                let inherited = self.preview(scope, key, None, task);
                // A value the old `config.toml` still supplies is not a settings
                // layer: setting it to the same word must write the new layer, or
                // the person's choice is never stored.
                let same = inherited.get(key).map(|sourced| &sourced.value) == Some(&value)
                    && inherited.source_of(key) != Some(Source::Legacy);
                if same {
                    (WriteOutcome::EqualToInherited, None)
                } else {
                    (WriteOutcome::Stored, Some(value))
                }
            }
        };
        let resolved = self.preview(scope, key, decided.as_ref(), task);
        // Nothing is written when the file already says what it should say: an
        // unset of a key nobody set must not create the file it would edit.
        let should_write = match (&decided, &current) {
            (None, None) => false,
            (Some(new), Some(old)) => new != old,
            _ => true,
        };
        if should_write {
            store::edit(
                &user_file(home),
                scope,
                key,
                decided.as_ref(),
                self.settings_text.as_deref(),
            )?;
        }
        Ok(Written { outcome, resolved })
    }
}

/// Put one change into a layer, or take it out.
fn apply(layer: &mut Layer, key: &Key, change: Option<&Value>) {
    match change {
        Some(value) => {
            layer.set(key.clone(), value.clone());
        }
        None => {
            layer.remove(key);
        }
    }
}

/// What a write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The value is now in the scope's layer.
    Stored,
    /// The value equals what the scope would inherit, so nothing was stored.
    EqualToInherited,
    /// The key was removed from the scope's layer.
    Removed,
    /// The key was not in the scope's layer, so there was nothing to remove.
    WasUnset,
}

impl WriteOutcome {
    /// The name the API and the JSON output use.
    pub fn name(self) -> &'static str {
        match self {
            Self::Stored => "stored",
            Self::EqualToInherited => "equal_to_inherited",
            Self::Removed => "removed",
            Self::WasUnset => "was_unset",
        }
    }

    /// A line saying what happened, for the verb's output.
    pub fn message(self, key: &Key) -> String {
        match self {
            Self::Stored => format!("ok {} stored", key.name()),
            Self::EqualToInherited => format!(
                "ok {} is back to what this scope inherits: it is no longer overridden here, and any earlier override was removed",
                key.name()
            ),
            Self::Removed => format!("ok {} unset", key.name()),
            Self::WasUnset => format!("ok {} was already unset", key.name()),
        }
    }
}

/// The settings a write left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// What the write did.
    pub outcome: WriteOutcome,
    /// The settings as they now resolve.
    pub resolved: Resolved,
}

/// Which layer a write changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// The person's defaults.
    You,
    /// The person's entry for one repository, by name.
    Repo(String),
}

mod store;

#[cfg(test)]
mod tests;
