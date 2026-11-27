// The fresh start a new worktree branches from is adapted from shepherdr (MIT),
// mods/tasks/bin/herdr_task/launch.py (fresh_start), commit a3f327a.
// Copyright (c) 2026 Elias Stravik
// https://github.com/eliasstravik/shepherdr
//
//! Per-attempt git worktrees, and the only removal path shprd may take.
//!
//! A worktree is created on a branch inside the layout's namespace and removed
//! only when its branch still starts with that namespace, so shprd can never
//! delete a branch a human owns.

use std::path::{Path, PathBuf};

use thiserror::Error;

use shprd_core::settings::{Key, Loaded, Source, TaskKey, Tasks};

use crate::git::{Git, GitError};

/// Where worktrees live, and what their branches are called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// Directory holding every worktree.
    pub root: PathBuf,
    /// Prefix every shprd branch carries.
    pub namespace: String,
    /// What follows the namespace in a branch's name, with `{run}`, `{task}`,
    /// `{n}` and `{slug}` filled in (`tasks.branch`). [`None`] names a branch
    /// `<run>/<task>`.
    pub pattern: Option<String>,
}

impl Layout {
    /// The default layout for a home directory: `<home>/.shprd/wt`, branches
    /// prefixed `shprd/`.
    pub fn default_for_home(home: impl Into<PathBuf>) -> Self {
        Self {
            root: home.into().join(".shprd").join("wt"),
            namespace: "shprd/".to_string(),
            pattern: None,
        }
    }

    /// The layout the settings describe for `repo`, read afresh: the namespace is
    /// `tasks.archive_namespace` and the branch pattern `tasks.branch`.
    ///
    /// Every verb and every pass builds its layout here, so a change saved in the
    /// settings is the next branch's name and the next janitor's prefix. A
    /// settings file that cannot be read leaves the default layout.
    pub fn from_settings(home: &Path, repo: &Path) -> Self {
        let layout = Self::default_for_home(home);
        // The old `config.toml` files (the person's, then the repository's own)
        // stand where no settings layer names the namespace: a repository's file
        // predates the settings model and is not one of its layers.
        let global = home.join(".config").join("shprd").join("config.toml");
        let local = repo.join(".shprd").join("config.toml");
        let legacy = shprd_core::config::load(Some(&global), Some(&local))
            .map(|config| config.janitor.namespace)
            .unwrap_or_else(|_| layout.namespace.clone());
        let Ok(loaded) = Loaded::read(home, Some(repo)) else {
            return layout.with_settings(&legacy, "");
        };
        let resolved = loaded.resolve(None);
        let tasks = resolved.tasks();
        let named = resolved
            .get(&Key::Task(TaskKey::ArchiveNamespace))
            .is_some_and(|sourced| sourced.source != Source::BuiltIn);
        let namespace = if named {
            tasks.archive_namespace.as_str()
        } else {
            legacy.as_str()
        };
        layout.with_settings(namespace, &tasks.branch)
    }

    /// This layout with the resolved workflow values applied.
    #[must_use]
    pub fn for_tasks(self, tasks: &Tasks) -> Self {
        self.with_settings(&tasks.archive_namespace, &tasks.branch)
    }

    /// This layout with the settings' namespace (`tasks.archive_namespace`) and
    /// branch pattern (`tasks.branch`) applied.
    ///
    /// A namespace that does not end in `/` gets one, and an empty one keeps the
    /// current namespace, so the janitor can never be handed a prefix that
    /// matches branches shprd did not make. A pattern git could not take as part
    /// of a branch name is ignored, and the plain `<run>/<task>` name stands.
    #[must_use]
    pub fn with_settings(mut self, namespace: &str, pattern: &str) -> Self {
        let namespace = namespace.trim();
        if !namespace.is_empty() {
            self.namespace = if namespace.ends_with('/') {
                namespace.to_owned()
            } else {
                format!("{namespace}/")
            };
        }
        let pattern = pattern.trim();
        self.pattern = valid_pattern(pattern).then(|| pattern.to_owned());
        self
    }

    /// The branch a task attempt commits on: `<namespace><pattern>`, which is
    /// `<namespace><run>/<task>` when no pattern is set.
    ///
    /// `{n}` is the last run of digits in the task id (the whole id when it has
    /// none) and `{slug}` is the id lowercased with every other character a `-`,
    /// so the name follows from the ids alone and the janitor can recompute it.
    pub fn branch_name(&self, run: &str, task: &str) -> String {
        let Some(pattern) = &self.pattern else {
            return format!("{}{run}/{task}", self.namespace);
        };
        let reversed: String = task
            .chars()
            .rev()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(char::is_ascii_digit)
            .collect();
        let digits: String = reversed.chars().rev().collect();
        let n = if digits.is_empty() { task } else { &digits };
        let slug: String = task
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        let name = pattern
            .replace("{run}", run)
            .replace("{task}", task)
            .replace("{n}", n)
            .replace("{slug}", &slug);
        format!("{}{name}", self.namespace)
    }

    /// The directory a task attempt works in:
    /// `<root>/<repo_name>/<run>/<task>`.
    pub fn path(&self, repo_name: &str, run: &str, task: &str) -> PathBuf {
        self.root.join(repo_name).join(run).join(task)
    }

    /// True when `branch` belongs to shprd, and so may be removed by it.
    pub fn owns_branch(&self, branch: &str) -> bool {
        branch.starts_with(&self.namespace)
    }
}

/// True when `pattern` can be the tail of a branch name once its tokens are
/// filled in: non-empty, no whitespace or characters git refuses, no empty,
/// dotted or `.lock` components, and a `{task}` or `{slug}` so two tasks never
/// share a branch.
fn valid_pattern(pattern: &str) -> bool {
    if pattern.is_empty() || pattern.starts_with('/') || pattern.ends_with('/') {
        return false;
    }
    if pattern.contains("..") || pattern.contains("//") || pattern.contains("@{") {
        return false;
    }
    // Two tasks must never share a branch: the id or its slug has to be in the
    // name (`{n}` alone is the same for `T1` and `SHPR-1`).
    if !pattern.contains("{task}") && !pattern.contains("{slug}") {
        return false;
    }
    let mut rest = pattern.to_owned();
    for token in ["{run}", "{task}", "{n}", "{slug}"] {
        rest = rest.replace(token, "x");
    }
    if rest
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\{}".contains(c))
    {
        return false;
    }
    rest.split('/')
        .all(|part| !part.starts_with('.') && !part.ends_with('.') && !part.ends_with(".lock"))
}

/// A worktree shprd created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Where it is on disk.
    pub path: PathBuf,
    /// The branch checked out in it.
    pub branch: String,
    /// The commit it was cut from: the base branch's fetched remote tip when
    /// the remote had one, else the local branch's.
    pub base_sha: String,
}

/// Why a worktree could not be created or removed.
#[derive(Debug, Error)]
pub enum WorktreeError {
    /// git failed.
    #[error(transparent)]
    Git(#[from] GitError),
    /// A run or task id is not a plain identifier.
    #[error("{what} {id:?} is not a usable identifier")]
    InvalidId {
        /// Which identifier was bad, e.g. `"run"`.
        what: &'static str,
        /// The value, as given.
        id: String,
    },
    /// A branch of that name already exists.
    #[error("branch {branch:?} already exists")]
    BranchExists {
        /// The branch name.
        branch: String,
    },
    /// Something already occupies the worktree path.
    #[error("worktree path {path:?} already exists")]
    PathExists {
        /// The path.
        path: PathBuf,
    },
    /// The branch does not belong to shprd, so shprd may not remove it.
    #[error("branch {branch:?} is outside the {namespace:?} namespace")]
    OutsideNamespace {
        /// The branch that was offered for removal.
        branch: String,
        /// shprd's own prefix.
        namespace: String,
    },
    /// The worktree path is not inside the layout root, so shprd may not
    /// remove it: it was not created by shprd.
    #[error("worktree path {path:?} is outside the root {root:?}")]
    OutsideRoot {
        /// The path that was offered for removal.
        path: PathBuf,
        /// shprd's worktree root.
        root: PathBuf,
    },
    /// A directory could not be created.
    #[error("could not create {path:?}: {source}")]
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The worktree has uncommitted work.
    #[error("worktree {path:?} has uncommitted changes; pass force to discard them")]
    Dirty {
        /// The worktree path.
        path: PathBuf,
    },
}

/// The directories shprd writes into the root of a checkout, which belong to
/// the run and never to the repository: `.shprd/` (the log, config and asks)
/// and `.dagr/` (the projected graph).
pub const STATE_DIRS: [&str; 2] = [".shprd/", ".dagr/"];

/// The comment written above [`STATE_DIRS`] in git's exclude file.
const EXCLUDE_COMMENT: &str = "# shprd: the run's own state, never committed";

/// Creates a worktree for one task attempt of `run` in `repo`.
///
/// The branch starts where [`fresh_start`] says: the base branch's fetched
/// remote tip when the remote holds everything the local branch has, so a task
/// branches from the latest even when others merged on the forge. The
/// repository's own checkout is never moved.
///
/// Refuses when the branch or the path already exists: recreating over either
/// would either lose someone else's commits or adopt work shprd did not make.
pub fn create(
    repo: &Path,
    layout: &Layout,
    run: &str,
    task: &str,
    base: &str,
) -> Result<Worktree, WorktreeError> {
    check_id("run", run)?;
    check_id("task", task)?;
    let git = Git::new(repo);
    let branch = layout.branch_name(run, task);
    let repo_name = repo_name(repo)?;
    let path = layout.path(&repo_name, run, task);
    if git.branch_exists(&branch)? {
        return Err(WorktreeError::BranchExists { branch });
    }
    if path.exists() {
        return Err(WorktreeError::PathExists { path });
    }
    let base_sha = git.rev_parse(&fresh_start(&git, base))?;
    if let Some(parent) = path.parent()
        && let Err(source) = std::fs::create_dir_all(parent)
    {
        return Err(WorktreeError::Io {
            path: parent.to_path_buf(),
            source,
        });
    }
    let path_text = path_string(&path)?;
    git.run(&["worktree", "add", "-b", &branch, &path_text, &base_sha])?;
    // A worker must never see shprd's own state as an untracked change, and the
    // repository's tracked `.gitignore` is not shprd's to edit: git's own
    // per-repository exclude list is. A failure undoes the worktree, so a create
    // that returns an error leaves nothing behind.
    if let Err(err) = ensure_state_excluded(&path) {
        let _ = git.run(&["worktree", "remove", "--force", &path_text]);
        let _ = git.run(&["branch", "-D", &branch]);
        return Err(err);
    }
    Ok(Worktree {
        path,
        branch,
        base_sha,
    })
}

/// Makes sure the repository a checkout belongs to excludes [`STATE_DIRS`].
///
/// The file is git's own per-repository exclude list
/// (`<git-common-dir>/info/exclude`): git never tracks it, every worktree of
/// the repository reads it, and the repository's tracked `.gitignore` is left
/// untouched. Only the entries that are missing are added, so calling this on a
/// checkout whose repository already excludes the state writes nothing, and a
/// person's own entries are kept exactly as they were.
///
/// [`create`] calls it for every worktree it makes, and a caller that *reuses* a
/// worktree — a hand-off, an integration worktree, every tick pass over the live
/// worktrees — calls it too, because a worktree an older build made excludes
/// nothing and its worker would see shprd's own state as an untracked change.
/// Being idempotent, it is safe to call on every pass.
pub fn ensure_state_excluded(worktree: &Path) -> Result<(), WorktreeError> {
    let git = Git::new(worktree);
    let listed = git.run(&["rev-parse", "--git-path", "info/exclude"])?;
    // git may print the path relative to the worktree or absolute; joining
    // keeps both readings.
    let path = worktree.join(listed.trim());
    let current = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => return Err(WorktreeError::Io { path, source }),
    };
    let missing: Vec<String> = STATE_DIRS
        .iter()
        .map(|dir| format!("/{dir}"))
        .filter(|pattern| !current.lines().any(|line| line.trim() == pattern))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let mut text = current;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.lines().any(|line| line.trim() == EXCLUDE_COMMENT) {
        text.push_str(EXCLUDE_COMMENT);
        text.push('\n');
    }
    for pattern in missing {
        text.push_str(&pattern);
        text.push('\n');
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| WorktreeError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(&path, text).map_err(|source| WorktreeError::Io { path, source })
}

/// Whether the repository has a remote a landing could push to.
///
/// A repository with no remote lands locally: its task branches are merged into
/// the run's integration branch in shprd's own worktree.
pub fn has_remote(git: &Git) -> bool {
    remote_of(git).is_some()
}

/// The host each of the repository's remotes points at, in `git remote` order.
///
/// A remote that is a path, a `file://` URL or anything else without a host
/// (a local bare repository, say) has no entry: there is no forge behind it.
pub fn remote_hosts(git: &Git) -> Vec<String> {
    let Ok(names) = git.run(&["remote"]) else {
        return Vec::new();
    };
    let mut hosts = Vec::new();
    for name in names.lines().map(str::trim).filter(|name| !name.is_empty()) {
        let Ok(url) = git.run(&["remote", "get-url", name]) else {
            continue;
        };
        if let Some(host) = host_of_url(&url)
            && !hosts.contains(&host)
        {
            hosts.push(host);
        }
    }
    hosts
}

/// The host of a git remote URL, lower-cased: `https://h/…`, `ssh://git@h/…` or
/// scp-style `git@h:owner/repo`. A path or `file://` URL has none.
pub fn host_of_url(url: &str) -> Option<String> {
    let url = url.trim();
    let authority = if let Some((scheme, rest)) = url.split_once("://") {
        if scheme.eq_ignore_ascii_case("file") {
            return None;
        }
        rest.split('/').next()?
    } else if url.starts_with('/') || url.starts_with('.') || url.starts_with('~') {
        return None;
    } else {
        // scp-style: `[user@]host:path`; a colon must come before any slash.
        let (host_part, _) = url.split_once(':')?;
        if host_part.contains('/') {
            return None;
        }
        host_part
    };
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?.trim().to_ascii_lowercase();
    // A Windows drive letter (`C:\repo`) is a path, not a host.
    (host.len() > 1 && !host.is_empty()).then_some(host)
}

/// The remote a landing fetches from and pushes to: `origin` when the
/// repository has it, else the only remote it has, else `None`.
///
/// Shepherdr reads the remotes the same way, and a repository with no remote at
/// all simply branches from its local base.
pub(crate) fn remote_of(git: &Git) -> Option<String> {
    let remotes = git.run(&["remote"]).ok()?;
    let names: Vec<&str> = remotes
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    if names.contains(&"origin") {
        return Some(String::from("origin"));
    }
    names.first().map(|name| (*name).to_string())
}

/// Where a new worktree on `base` starts.
///
/// The base is fetched from the remote first, and the remote's tip is used when
/// it holds everything the local branch has — which is the ordinary case, where
/// the local branch is only behind. The local branch is used instead when it has
/// work of its own that the remote lacks, when there is no remote, or when the
/// remote has no such branch: a task then branches from what the repository
/// actually has, and nothing is moved behind a human's back.
///
/// This is Shepherdr's `fresh_start` (MIT): a task branches from the latest
/// base, even when other work merged on the forge since the checkout was last
/// fetched.
pub fn fresh_start(git: &Git, base: &str) -> String {
    let Some(remote) = remote_of(git) else {
        return base.to_string();
    };
    if !git.branch_exists(base).unwrap_or(false) {
        return base.to_string();
    }
    let tracking = format!("refs/remotes/{remote}/{base}");
    let fetch = git.run(&[
        "fetch",
        "--quiet",
        "--no-tags",
        &remote,
        &format!("+refs/heads/{base}:{tracking}"),
    ]);
    if fetch.is_err() || git.rev_parse(&tracking).is_err() {
        return base.to_string();
    }
    let behind = git
        .run(&["merge-base", "--is-ancestor", base, &tracking])
        .is_ok();
    // With a forge, origin is the truth even when the local ref has commits of
    // its own: those are a landing that never reached the forge.
    if behind || crate::land::forge_expected(git) {
        format!("{remote}/{base}")
    } else {
        base.to_string()
    }
}

/// Removes a worktree and, with it, the branch it has checked out.
///
/// Refuses any branch outside [`Layout::namespace`], so a branch a human made
/// by hand is never touched, and otherwise does exactly what
/// [`remove_worktree_only`] does. Only a caller whose ledger recorded the
/// branch may take this path; the janitor does not, because a worktree can
/// exist for a branch the ledger never recorded.
pub fn remove(
    repo: &Path,
    layout: &Layout,
    wt: &Worktree,
    force: bool,
) -> Result<(), WorktreeError> {
    if !layout.owns_branch(&wt.branch) {
        return Err(WorktreeError::OutsideNamespace {
            branch: wt.branch.clone(),
            namespace: layout.namespace.clone(),
        });
    }
    remove_worktree_only(repo, layout, &wt.path, force)?;
    Git::new(repo).run(&["branch", "-D", &wt.branch])?;
    Ok(())
}

/// Removes a worktree and leaves its branch alone.
///
/// Refuses a path outside [`Layout::root`], so a directory shprd did not
/// create is never deleted, and refuses a dirty worktree unless `force`. No
/// branch is ever deleted here: `git worktree remove` keeps the branch, and a
/// branch is only removed by a step the ledger planned from a recorded
/// `branch` resource.
pub fn remove_worktree_only(
    repo: &Path,
    layout: &Layout,
    path: &Path,
    force: bool,
) -> Result<(), WorktreeError> {
    if !inside_root(&layout.root, path) {
        return Err(WorktreeError::OutsideRoot {
            path: path.to_path_buf(),
            root: layout.root.clone(),
        });
    }
    if !force && path.exists() && !Git::new(path).is_clean()? {
        return Err(WorktreeError::Dirty {
            path: path.to_path_buf(),
        });
    }
    let git = Git::new(repo);
    let path_text = path_string(path)?;
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&path_text);
    if let Err(err) = git.run(&args) {
        // `git worktree remove` fails half-way when something is still writing
        // into the tree (a build's output, a process that has not quit) and says
        // the directory is not empty, or when the directory is already gone and
        // only git's record is left. Both are finished by hand: a forced removal
        // deletes the folder itself, and either way git's record is pruned.
        if !force && path.exists() {
            return Err(err.into());
        }
        if path.exists() && std::fs::remove_dir_all(path).is_err() {
            return Err(err.into());
        }
        git.run(&["worktree", "prune"])?;
        if path.exists() {
            return Err(err.into());
        }
    }
    Ok(())
}

/// Deletes one branch, treating one that is already gone as gone.
///
/// Refuses any branch outside [`Layout::namespace`], so a branch a human made
/// by hand is never deleted. This is the only branch deletion shprd has, and
/// callers reach it only for a branch their ledger recorded creating.
pub fn delete_branch(repo: &Path, layout: &Layout, branch: &str) -> Result<(), WorktreeError> {
    if !layout.owns_branch(branch) {
        return Err(WorktreeError::OutsideNamespace {
            branch: branch.to_string(),
            namespace: layout.namespace.clone(),
        });
    }
    let git = Git::new(repo);
    if !git.branch_exists(branch)? {
        return Ok(());
    }
    git.run(&["branch", "-D", branch])?;
    Ok(())
}

/// The worktree that has `branch` checked out, when one does.
///
/// Git lets a branch be checked out in one worktree at a time, the person's own
/// checkout included, so this is where shprd looks before it needs the branch
/// for itself. `None` when no worktree holds it, or when git cannot say.
pub fn checked_out_at(git: &Git, branch: &str) -> Option<PathBuf> {
    let listing = git.run(&["worktree", "list", "--porcelain"]).ok()?;
    let wanted = format!("refs/heads/{branch}");
    let mut path: Option<PathBuf> = None;
    for line in listing.lines() {
        if let Some(found) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(found));
        } else if line.strip_prefix("branch ") == Some(wanted.as_str()) {
            return path;
        }
    }
    None
}

/// Rejects an id that is empty, uses anything but `[A-Za-z0-9._-]`, or
/// contains `..`.
fn check_id(what: &'static str, id: &str) -> Result<(), WorktreeError> {
    if valid_id(id) {
        return Ok(());
    }
    Err(WorktreeError::InvalidId {
        what,
        id: id.to_string(),
    })
}

/// True when `id` is usable as a run or task id: non-empty, only
/// `[A-Za-z0-9._-]`, and no `..`.
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// True when `path` lies inside `root`, compared by path components rather
/// than by string prefix.
///
/// Both sides are resolved first — symlinks and `..` dropped, for the longest
/// existing prefix of the path — so `/tmp/x/wt` and `/private/tmp/x/wt` are the
/// same place on a platform where `/tmp` is a symlink.
pub(crate) fn inside_root(root: &Path, path: &Path) -> bool {
    resolve(path).starts_with(resolve(root))
}

/// Replaces `path` with an absolute, symlink-free path, resolving as much of
/// it as exists and appending the rest unchanged.
fn resolve(path: &Path) -> PathBuf {
    let mut prefix = path.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(mut resolved) = std::fs::canonicalize(&prefix) {
            for part in rest.iter().rev() {
                resolved.push(part);
            }
            return resolved;
        }
        match (
            prefix.file_name().map(|name| name.to_os_string()),
            prefix.parent(),
        ) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                prefix = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// The name of the repository directory, used to keep repos apart on disk.
fn repo_name(repo: &Path) -> Result<String, WorktreeError> {
    match repo.file_name().and_then(|name| name.to_str()) {
        Some(name) if !name.is_empty() => Ok(name.to_string()),
        _ => Err(WorktreeError::InvalidId {
            what: "repository",
            id: repo.display().to_string(),
        }),
    }
}

/// A path as git should see it.
fn path_string(path: &Path) -> Result<String, WorktreeError> {
    match path.to_str() {
        Some(text) => Ok(text.to_string()),
        None => Err(WorktreeError::InvalidId {
            what: "path",
            id: path.display().to_string(),
        }),
    }
}

/// How one `tasks.worktree_files` entry reaches a worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedMode {
    /// The file is copied, so the worker's edits stay in its worktree.
    Copy,
    /// The file is a symlink to the main checkout's, so both see one file.
    Link,
}

/// One parsed `tasks.worktree_files` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedEntry {
    /// The path relative to the repository root.
    pub path: String,
    /// Whether it is copied or linked.
    pub mode: SeedMode,
}

/// What seeding one entry did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedOutcome {
    /// The entry reached the worktree.
    Placed(SeedEntry),
    /// The entry did not, for the reason given; the worker will not see it.
    Skipped(SeedEntry, String),
}

/// Reads a `tasks.worktree_files` value: entries separated by newlines or
/// commas, a leading `link:` making an entry a symlink rather than a copy.
/// Blank entries are dropped.
pub fn parse_seed_entries(text: &str) -> Vec<SeedEntry> {
    text.split(['\n', ','])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| match entry.strip_prefix("link:") {
            Some(path) => SeedEntry {
                path: path.trim().to_owned(),
                mode: SeedMode::Link,
            },
            None => SeedEntry {
                path: entry.to_owned(),
                mode: SeedMode::Copy,
            },
        })
        .collect()
}

/// Why `path` may not be seeded, or [`None`] when it may: it must stay inside
/// the repository and clear of git's and shprd's own directories.
fn seed_refusal(path: &str) -> Option<&'static str> {
    let path = Path::new(path);
    let mut first = true;
    for part in path.components() {
        match part {
            std::path::Component::Normal(name) => {
                // `.git` is refused at any depth (a nested repository's
                // metadata); shprd's own directories only at the top.
                if name == ".git" || (first && (name == ".shprd" || name == ".dagr")) {
                    return Some("a state directory is never seeded");
                }
            }
            std::path::Component::CurDir => continue,
            _ => return Some("the path must be relative and stay inside the repository"),
        }
        first = false;
    }
    if path.as_os_str().is_empty() {
        return Some("the path is empty");
    }
    None
}

/// Copies a file or a directory tree.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

/// Copies or links the `entries` from the main checkout `repo` into
/// `worktree` (`tasks.worktree_files`).
///
/// An entry that is missing in the main checkout, already present in the
/// worktree (a tracked file wins), outside the repository, or that cannot be
/// written is skipped with its reason and never fails the spawn.
pub fn seed_files(repo: &Path, worktree: &Path, entries: &[SeedEntry]) -> Vec<SeedOutcome> {
    entries
        .iter()
        .map(|entry| {
            let skip = |why: String| SeedOutcome::Skipped(entry.clone(), why);
            if let Some(why) = seed_refusal(&entry.path) {
                return skip(why.to_owned());
            }
            let from = repo.join(&entry.path);
            let to = worktree.join(&entry.path);
            if std::fs::symlink_metadata(&from).is_err() {
                return skip("it is not in the main checkout".to_owned());
            }
            if std::fs::symlink_metadata(&to).is_ok() {
                return skip("the worktree already has it".to_owned());
            }
            if let Some(parent) = to.parent()
                && let Err(err) = std::fs::create_dir_all(parent)
            {
                return skip(format!("cannot make {}: {err}", parent.display()));
            }
            let placed = match entry.mode {
                SeedMode::Copy => copy_tree(&from, &to),
                #[cfg(unix)]
                SeedMode::Link => std::os::unix::fs::symlink(&from, &to),
                #[cfg(not(unix))]
                SeedMode::Link => copy_tree(&from, &to),
            };
            match placed {
                Ok(()) => SeedOutcome::Placed(entry.clone()),
                Err(err) => skip(err.to_string()),
            }
        })
        .collect()
}

/// The untracked and git-ignored paths of `repo` that any of `texts` (a task's
/// scope and words) names, minus those `seeded` already provides.
///
/// A worker's worktree is cut from a branch, so these are silently absent
/// there. A directory is listed whole (`docs/`) and matches a text that names
/// anything beneath it. Returns an empty list when git cannot say.
pub fn unseen_mentions(repo: &Path, texts: &[&str], seeded: &[SeedEntry]) -> Vec<String> {
    let git = Git::new(repo);
    let mut found: Vec<String> = Vec::new();
    for ignored in [false, true] {
        let mut args = vec!["ls-files", "--others", "--exclude-standard", "--directory"];
        if ignored {
            args.push("--ignored");
        }
        let Ok(listed) = git.run(&args) else {
            continue;
        };
        for path in listed
            .lines()
            .map(str::trim)
            .filter(|path| !path.is_empty())
        {
            if found.iter().any(|known| known == path) {
                continue;
            }
            let bare = path.trim_end_matches('/');
            let covered = seeded.iter().any(|entry| {
                let entry = entry.path.trim_end_matches('/');
                bare == entry || bare.starts_with(&format!("{entry}/"))
            });
            if !covered && texts.iter().any(|text| names_path(text, path)) {
                found.push(path.to_owned());
            }
        }
    }
    found
}

/// Whether `text` names `path`: the whole path, not the tail of a longer one.
fn names_path(text: &str, path: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/');
    // A directory path ends in `/`, which is its own boundary.
    let dir = path.ends_with('/');
    text.match_indices(path).any(|(at, _)| {
        let before = text[..at].chars().next_back().is_none_or(|c| !word(c));
        let rest = &text[at + path.len()..];
        let mut chars = rest.chars();
        let after = dir
            || match chars.next() {
                None => true,
                // a sentence's full stop is not part of the name
                Some('.') => chars.next().is_none_or(|c| !word(c)),
                Some(c) => !word(c),
            };
        before && after
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A repository with one commit on `main`, and a home outside it.
    fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
        let repo = tempfile::tempdir().expect("repo dir");
        let home = tempfile::tempdir().expect("home dir");
        let git = Git::new(repo.path());
        git.run(&["init", "-b", "main"]).expect("git init");
        git.run(&["config", "user.name", "shprd tests"])
            .expect("user.name");
        git.run(&["config", "user.email", "tests@example.invalid"])
            .expect("user.email");
        git.run(&["config", "commit.gpgsign", "false"])
            .expect("gpgsign");
        std::fs::write(
            repo.path().join("calc.py"),
            "def add(a, b):\n    return a + b\n",
        )
        .expect("a seed file");
        git.run(&["add", "--", "calc.py"]).expect("git add");
        git.run(&["commit", "-m", "seed"]).expect("git commit");
        (repo, home)
    }

    /// The repository's own exclude file, as git points at it.
    fn exclude_of(repo: &Path) -> PathBuf {
        let listed = Git::new(repo)
            .run(&["rev-parse", "--git-path", "info/exclude"])
            .expect("git path");
        repo.join(listed.trim())
    }

    #[test]
    fn seed_refusal_covers_nested_git_and_names_path_trailing_boundary() {
        assert!(seed_refusal("vendor/repo/.git/config").is_some());
        assert!(seed_refusal("config/.env").is_none());
        assert!(names_path("edit .env now", ".env"));
        assert!(names_path("see .env.", ".env"));
        assert!(!names_path("edit .env.bak", ".env"));
        assert!(!names_path("in secrets2/x", "secrets/"));
        assert!(names_path("in secrets/x", "secrets/"));
    }

    #[test]
    fn worktree_files_are_copied_and_linked_into_a_new_worktree() {
        let (repo, home) = fixture();
        std::fs::write(repo.path().join(".gitignore"), ".env\nsecrets/\n").unwrap();
        let git = Git::new(repo.path());
        git.run(&["add", ".gitignore"]).unwrap();
        git.run(&["commit", "-m", "ignore"]).unwrap();
        std::fs::write(repo.path().join(".env"), "KEY=1\n").unwrap();
        std::fs::create_dir_all(repo.path().join("secrets")).unwrap();
        std::fs::write(repo.path().join("secrets/a.txt"), "a").unwrap();
        let layout = Layout::default_for_home(home.path());
        let created = create(repo.path(), &layout, "m9", "T1", "main").unwrap();
        let entries = parse_seed_entries(".env\nlink:secrets, missing.txt,../escape, calc.py");
        let outcomes = seed_files(repo.path(), &created.path, &entries);
        assert_eq!(outcomes.len(), 5);
        assert!(matches!(outcomes[0], SeedOutcome::Placed(_)));
        assert!(matches!(outcomes[1], SeedOutcome::Placed(_)));
        assert!(
            matches!(&outcomes[2], SeedOutcome::Skipped(_, why) if why.contains("main checkout"))
        );
        assert!(matches!(&outcomes[3], SeedOutcome::Skipped(_, why) if why.contains("inside")));
        assert!(matches!(&outcomes[4], SeedOutcome::Skipped(_, why) if why.contains("already")));
        let env = created.path.join(".env");
        assert!(!env.is_symlink());
        assert_eq!(std::fs::read_to_string(env).unwrap(), "KEY=1\n");
        let linked = created.path.join("secrets");
        assert!(linked.is_symlink());
        assert_eq!(std::fs::read_to_string(linked.join("a.txt")).unwrap(), "a");
        assert!(!created.path.parent().unwrap().join("escape").exists());
    }

    #[test]
    fn untracked_and_ignored_files_a_task_names_are_listed() {
        let (repo, _home) = fixture();
        std::fs::write(repo.path().join(".gitignore"), ".env\n").unwrap();
        let git = Git::new(repo.path());
        git.run(&["add", ".gitignore"]).unwrap();
        git.run(&["commit", "-m", "ignore"]).unwrap();
        std::fs::write(repo.path().join(".env"), "K=1").unwrap();
        std::fs::create_dir_all(repo.path().join("docs")).unwrap();
        std::fs::write(repo.path().join("docs/new.md"), "x").unwrap();
        std::fs::write(repo.path().join("other.txt"), "x").unwrap();
        let texts = ["read docs/new.md and run with .env", "calc.py"];
        let none: Vec<SeedEntry> = Vec::new();
        let mut found = unseen_mentions(repo.path(), &texts, &none);
        found.sort();
        assert_eq!(found, vec![".env".to_owned(), "docs/".to_owned()]);
        let seeded = parse_seed_entries(".env");
        assert_eq!(
            unseen_mentions(repo.path(), &texts, &seeded),
            vec!["docs/".to_owned()]
        );
        assert!(unseen_mentions(repo.path(), &["myother.txt"], &none).is_empty());
    }

    #[test]
    fn a_new_worktree_excludes_shprds_state_directories() {
        let (repo, home) = fixture();
        let layout = Layout::default_for_home(home.path());
        let created = create(repo.path(), &layout, "m9", "T1", "main").expect("a worktree");
        let text = std::fs::read_to_string(exclude_of(repo.path())).expect("the exclude file");
        for pattern in ["/.shprd/", "/.dagr/"] {
            assert!(
                text.lines().any(|line| line.trim() == pattern),
                "{pattern} is missing from:\n{text}"
            );
        }

        // What the engine and a worker's own verbs write there no longer shows
        // up as an untracked change in the worker's checkout.
        std::fs::create_dir_all(created.path.join(".shprd")).expect("state dir");
        std::fs::create_dir_all(created.path.join(".dagr")).expect("graph dir");
        std::fs::write(created.path.join(".shprd/events.jsonl"), "{}\n").expect("an event");
        std::fs::write(created.path.join(".dagr/run.json"), "{}\n").expect("a graph");
        assert!(
            Git::new(&created.path).is_clean().expect("git status"),
            "shprd's own state must not show as a change"
        );

        // The repository's tracked ignore file is never written: only git's own
        // untracked, per-repository exclude list is.
        assert!(
            !repo.path().join(".gitignore").exists(),
            "the repository's .gitignore is not shprd's to edit"
        );
    }

    #[test]
    fn a_second_worktree_does_not_duplicate_the_exclusions() {
        let (repo, home) = fixture();
        let layout = Layout::default_for_home(home.path());
        create(repo.path(), &layout, "m9", "T1", "main").expect("the first worktree");
        create(repo.path(), &layout, "m9", "T2", "main").expect("the second worktree");
        let text = std::fs::read_to_string(exclude_of(repo.path())).expect("the exclude file");
        for pattern in ["/.shprd/", "/.dagr/"] {
            assert_eq!(
                text.lines().filter(|line| line.trim() == pattern).count(),
                1,
                "{pattern} appears once in:\n{text}"
            );
        }
    }

    /// A worktree an older build made (or one a human restored) has no
    /// exclusions; the repair adds them without touching anything else.
    #[test]
    fn a_worktree_made_before_the_change_is_repaired() {
        let (repo, home) = fixture();
        let layout = Layout::default_for_home(home.path());
        let created = create(repo.path(), &layout, "m9", "T1", "main").expect("a worktree");
        // Simulate the old state: the entries removed again.
        let path = exclude_of(repo.path());
        std::fs::write(&path, "").expect("an empty exclude file");

        ensure_state_excluded(&created.path).expect("the repair runs");
        let text = std::fs::read_to_string(&path).expect("the exclude file");
        for pattern in ["/.shprd/", "/.dagr/"] {
            assert!(
                text.lines().any(|line| line.trim() == pattern),
                "{pattern} was not repaired into:\n{text}"
            );
        }

        // And what the worker writes there is invisible to git again.
        std::fs::create_dir_all(created.path.join(".shprd")).expect("state dir");
        std::fs::write(created.path.join(".shprd/events.jsonl"), "{}\n").expect("an event");
        assert!(
            Git::new(&created.path).is_clean().expect("git status"),
            "the repaired worktree still shows the state"
        );
    }

    /// The repair is idempotent: running it on a worktree that already excludes
    /// the state changes nothing, so a caller may run it on every pass.
    #[test]
    fn the_repair_is_idempotent() {
        let (repo, home) = fixture();
        let layout = Layout::default_for_home(home.path());
        let created = create(repo.path(), &layout, "m9", "T1", "main").expect("a worktree");
        let path = exclude_of(repo.path());
        let before = std::fs::read_to_string(&path).expect("the exclude file");
        ensure_state_excluded(&created.path).expect("the first repair");
        ensure_state_excluded(&created.path).expect("the second repair");
        let after = std::fs::read_to_string(&path).expect("the exclude file");
        assert_eq!(before, after, "a repeated repair rewrote the file");
    }

    #[test]
    fn a_persons_own_exclude_entries_are_kept() {
        let (repo, home) = fixture();
        let path = exclude_of(repo.path());
        std::fs::create_dir_all(path.parent().expect("the info dir")).expect("info dir");
        std::fs::write(&path, "# mine\n*.local\n").expect("the person's entries");
        let layout = Layout::default_for_home(home.path());
        create(repo.path(), &layout, "m9", "T1", "main").expect("a worktree");
        let text = std::fs::read_to_string(&path).expect("the exclude file");
        assert!(text.contains("# mine\n*.local\n"), "{text}");
        assert!(text.contains("/.shprd/"), "{text}");
        assert!(text.contains("/.dagr/"), "{text}");
    }

    #[test]
    fn the_branch_pattern_and_namespace_name_the_branch_and_the_worktree_uses_it() {
        let (repo, home) = fixture();
        let layout = Layout::default_for_home(home.path()).with_settings("work", "task/{n}-{slug}");
        assert_eq!(layout.branch_name("m9", "SHPR-24"), "work/task/24-shpr-24");
        let created = create(repo.path(), &layout, "m9", "SHPR-24", "main").expect("a worktree");
        assert_eq!(created.branch, "work/task/24-shpr-24");
        assert!(layout.owns_branch(&created.branch));
        // Unset, the name is what it always was.
        let plain = Layout::default_for_home(home.path()).with_settings("", "");
        assert_eq!(plain.branch_name("m9", "T1"), "shprd/m9/T1");
    }

    #[test]
    fn a_pattern_git_would_refuse_is_ignored() {
        for bad in [
            "a b",
            "x..y",
            "/x",
            "x/",
            "a:{task}",
            "{task}.lock",
            "a~{task}",
            "fixed",
            "{n}",
            "{run}",
        ] {
            let layout = Layout::default_for_home("/h").with_settings("shprd/", bad);
            assert_eq!(layout.branch_name("r", "T1"), "shprd/r/T1", "{bad}");
        }
    }
}
