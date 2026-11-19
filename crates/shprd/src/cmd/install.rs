//! `shprd install herdr-desktop`: put shprd's layout and mods into the Herdr
//! desktop's config folder, owning exactly the files it wrote.
//!
//! The Herdr app draws itself from `~/.config/herdr-app` (or `$HERDR_APP_HOME`):
//! `layouts/<name>.json`, `mods/<name>/`, `keys.json`. Installing shprd's desktop
//! therefore means copying files into a folder the person also keeps their own
//! work in, so the rules are the ones Shepherdr's installer follows
//! (`bin/shepherdr`, `plan_files` and `merge_keys`; MIT, eliasstravik/shepherdr,
//! commit a3f327a):
//!
//! - **Own only what we wrote.** A manifest under `<app home>/data/shprd-desktop/`
//!   lists every file and directory this verb created and a SHA-256 of each file's
//!   content. Nothing outside that list is touched.
//! - **Never clobber a file that is not ours.** A target path that already holds a
//!   file we did not write is a clash: the install refuses, names every clash, and
//!   writes nothing at all.
//! - **Merge keys, never overwrite one.** `keys.json` is merged per
//!   `(section, key)`: a key that is absent, or that still holds the value this
//!   verb wrote last time, takes shprd's command; a key the person bound to
//!   something else is kept and reported. Uninstall gives back only the keys that
//!   still hold our value.
//! - **Idempotent.** A re-install whose files and keys already match writes
//!   nothing and says so. An install after shprd changed updates only what moved.
//! - **`--dry-run` writes nothing** and prints the same plan an install would run.
//! - **Uninstall removes only the manifest's files**, and keeps any of them the
//!   person has edited since (a change is theirs, not stale ours).
//!
//! The source tree is the one this *binary* carries, not one the caller happens
//! to stand in: `--from`, then `$SHPRD_HOST_DIR`, then `--repo`'s
//! `host/herdr-desktop`, then the checkout this binary was compiled from in a
//! debug build, and finally the copy
//! embedded in the binary at build time (`build.rs`, the same shape as
//! `shprd-agents`'s `data/`). An installed `shprd` therefore installs its own
//! desktop with no checkout at all, and a worker's worktree installs the worktree
//! rather than the canonical checkout (the M10 gate's bug 6). The working
//! directory is never searched: an install run from an old checkout must not
//! install that checkout's old layout over the one this binary ships. Every path here is
//! carried in as a value; this module reads no environment variable itself.
//!
//! A file Herdr pruned is put back: Herdr resolves a layout terminal block by
//! name when the layout loads and rewrites the file without the block when no
//! such terminal exists (measured, `crates/shprd/src/cmd/graph.rs`). The prune is
//! the app's edit and not the person's, so an install whose manifest still owns
//! the file restores it — only a change that is not a prune is refused.
//!
//! A prune is also recognised across a layout's own change ([`PREVIOUS_LAYOUTS`]):
//! a build that adds or takes away a block ships a different layout, and a person
//! who installed the build before may be holding that older layout with a
//! terminal block pruned out of it. Refusing *that* would make every such upgrade
//! a manual repair, so the layouts earlier builds shipped are carried here as the
//! documents they were, and a file that is one of them with its terminal blocks
//! pruned is the app's edit rather than the person's.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cli::InstallArgs;
use crate::ctx::CliError;

include!(concat!(env!("OUT_DIR"), "/host_tree.rs"));

/// Where the manifest lives, relative to the app home.
const MANIFEST_DIR: &str = "data/shprd-desktop";
/// The folder `data/` under the app home, which holds [`MANIFEST_DIR`] and which
/// this verb removes on uninstall when it leaves it empty. Herdr writes its own
/// `data/shared.json` beside it, so a `data/` that held something else is kept.
const DATA_DIR: &str = "data";
/// The manifest's file name inside [`MANIFEST_DIR`].
const MANIFEST_FILE: &str = "manifest.json";
/// The version of the manifest's shape.
const MANIFEST_VERSION: u32 = 1;
/// The mod folder prefix this verb owns. A folder named otherwise is Herdr's or
/// the person's, and is never written to or removed.
const MOD_PREFIX: &str = "shprd-";

/// The layouts earlier builds shipped, as the documents they were.
///
/// One entry per version of a layout file this verb has installed that has since
/// changed — every version, because a person upgrades from whichever one they last
/// installed: a file Herdr pruned out of an *older* build's layout is still
/// recognised as the app's edit when this build's layout is a different document,
/// which is what makes such an upgrade one command rather than a manual repair. The
/// rule for a later change is to add here the layout it replaces, and nothing else
/// — a document here is a fact about what shprd shipped, never a guess about what a
/// person meant.
///
/// The three M10 layouts are the ones that differ (the four M10 fix releases did
/// not touch the file): M10.3's, M10.4's (`spaces` became `shprd-spaces`, and the
/// tab bar and panel gained their buttons) and M10.7's, which is where M10 ended
/// and whose side panel held settings as a tab. M11 fix S made settings a dialog
/// over the window, so that block went away. M11 U3 made the Accept card a dialog,
/// so the task thread's stack lost its `tasks-thread-accept` block and opens on the
/// terminal; the layout before that is `shprd-m11-menu.json` (and `shprd-m11.json` before the
/// terminal menu was added). M11 U4 gave the task thread a Chat view it opens on and
/// took the Asks stack and the sidebar's coordinator block away; the layout before
/// that is `shprd-m11-accept.json`. SHPR-28 gave the thread a read-only stream child
/// (the Terminal view's fallback) and an Artifacts child; the layout before that is
/// `shprd-m11-thread.json`.
const PREVIOUS_LAYOUTS: &[&str] = &[
    include_str!("previous-layouts/shprd-m10.3.json"),
    include_str!("previous-layouts/shprd-m10.4.json"),
    include_str!("previous-layouts/shprd-m10.json"),
    include_str!("previous-layouts/shprd-m11.json"),
    include_str!("previous-layouts/shprd-m11-menu.json"),
    include_str!("previous-layouts/shprd-m11-accept.json"),
    include_str!("previous-layouts/shprd-m11-thread.json"),
];

/// Runs `shprd install`, whichever mode the command line asked for.
pub fn run(args: &InstallArgs, home: Option<&Path>, json: bool) -> Result<(), CliError> {
    let app_home = resolve_app_home(args, home)?;
    if args.uninstall {
        return uninstall(&app_home, args.dry_run, json);
    }
    let source = resolve_source(args)?;
    install(&source, &app_home, args.dry_run, json)
}

/// The Herdr app home: `--app-home`, then `$HERDR_APP_HOME`, then
/// `<home>/.config/herdr-app`.
///
/// Herdr documents `~/.config/herdr-app` as its home and `HERDR_APP_HOME` as the
/// way to pick another; nothing else moves it. It is resolved here, once, and
/// carried as a value so a test can point it at a temporary folder and never
/// near the person's own window.
fn resolve_app_home(args: &InstallArgs, home: Option<&Path>) -> Result<PathBuf, CliError> {
    if let Some(path) = &args.app_home {
        return Ok(path.clone());
    }
    if let Some(value) = std::env::var_os("HERDR_APP_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    Ok(crate::ctx::resolve_home(home)?
        .join(".config")
        .join("herdr-app"))
}

/// Where the files to install come from.
///
/// Two answers, because there are two things a person can install: a host tree
/// they point at (their checkout, a worktree, a folder they edited), and the copy
/// this binary carries. The embedded copy is what makes `shprd install
/// herdr-desktop` work from a released binary with no checkout beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// A `host/herdr-desktop` directory.
    Tree(PathBuf),
    /// The copy embedded in this binary at build time.
    Embedded,
}

impl Source {
    /// Where the files came from, in words a person reads in the report.
    fn label(&self) -> String {
        match self {
            Source::Tree(path) => path.display().to_string(),
            Source::Embedded => String::from("embedded in this binary"),
        }
    }

    /// Every file in the tree, relative to its root, sorted.
    fn walk(&self) -> Result<Vec<String>, CliError> {
        match self {
            Source::Tree(root) => walk(root),
            Source::Embedded => Ok(HOST_FILES
                .iter()
                .map(|(path, _)| (*path).to_string())
                .collect()),
        }
    }

    /// One file's bytes, by its path in the tree.
    fn read(&self, rel: &str) -> Result<Vec<u8>, CliError> {
        match self {
            Source::Tree(root) => read_file(&root.join(rel)),
            Source::Embedded => read_embedded(rel),
        }
    }

    /// The bindings this tree's `keys.json` asks for, `section -> key -> command`.
    ///
    /// A tree with no `keys.json` asks for none, which is what a person who
    /// deleted theirs from a copy meant.
    fn keys(&self) -> Result<BTreeMap<String, BTreeMap<String, String>>, CliError> {
        match self {
            Source::Tree(root) => read_keys(&root.join("keys.json")),
            Source::Embedded => match HOST_FILES.iter().find(|(path, _)| *path == "keys.json") {
                Some((_, bytes)) => parse_keys(
                    std::str::from_utf8(bytes)
                        .map_err(|err| CliError::other(anyhow!(err).context("keys.json")))?,
                    Path::new("keys.json"),
                ),
                None => Ok(BTreeMap::new()),
            },
        }
    }
}

/// Whether a file in the tree is a layout Herdr may rewrite.
fn layout_file(rel: &str) -> bool {
    rel.starts_with("layouts/") && rel.ends_with(".json")
}

/// The tree to install from: `--from`, then `$SHPRD_HOST_DIR`, then a checkout
/// named by `--repo`, then the checkout this binary was built from in a debug
/// build, then the embedded copy.
///
/// A tree named outright (`--from`, `$SHPRD_HOST_DIR`) must be one: falling back
/// to the embedded copy would install something other than what was asked for.
/// The working directory and its parent are not searched.
fn resolve_source(args: &InstallArgs) -> Result<Source, CliError> {
    if let Some(path) = &args.from {
        return Ok(Source::Tree(checked_tree(path, "--from")?));
    }
    if let Some(value) = std::env::var_os("SHPRD_HOST_DIR").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(&value);
        return Ok(Source::Tree(checked_tree(&path, "$SHPRD_HOST_DIR")?));
    }
    for candidate in search_places(args) {
        if candidate.join("layouts").is_dir() {
            return Ok(Source::Tree(candidate));
        }
    }
    Ok(Source::Embedded)
}

/// The `host/herdr-desktop` folders to try, best first, for a caller who named
/// none.
///
/// The order is "what this binary is", not "where the person happens to be":
/// `--repo`, then — in a debug build only — the checkout the binary was compiled
/// from, which is the same tree by another name. A release build has no
/// such path to offer and says nothing about it.
fn search_places(args: &InstallArgs) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut push = |base: PathBuf| {
        let candidate = base.join("host").join("herdr-desktop");
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    };
    // `--repo` is the caller naming the checkout outright, so it comes first.
    if let Some(repo) = &args.repo {
        push(repo.clone());
    }
    if cfg!(debug_assertions) {
        push(Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".."));
    }
    out
}

/// `path` as a host tree, or a refusal naming how it was named and what it lacks.
fn checked_tree(path: &Path, named: &str) -> Result<PathBuf, CliError> {
    if path.join("layouts").is_dir() {
        return Ok(path.to_path_buf());
    }
    Err(CliError::other(anyhow!(
        "{} (from {named}) is not a Herdr desktop host tree: it has no `layouts/` directory",
        path.display()
    )))
}

/// The manifest: everything this verb wrote, so it can take back its own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    /// The shape's version, so a later one can migrate rather than guess.
    version: u32,
    /// Every installed file, relative to the app home, with its content's hash.
    files: BTreeMap<String, String>,
    /// Every directory install created, relative to the app home, so uninstall
    /// can take an empty one away and leave one the person filled.
    dirs: BTreeSet<String>,
    /// Every key this verb wrote: `section -> key -> the command it wrote`.
    ///
    /// Only a key that still holds this value is removed on uninstall, and only
    /// one that still holds it (or holds the previous one) is rewritten on
    /// install — a person's binding is never taken.
    keys: BTreeMap<String, BTreeMap<String, String>>,
}

impl Manifest {
    /// Reads the manifest, or an empty one when there is none.
    fn read(app_home: &Path) -> Result<Self, CliError> {
        let path = manifest_path(app_home);
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|err| {
                CliError::other(anyhow!(err).context(format!(
                    "{} is not a manifest this version understands; move it aside to reinstall",
                    path.display()
                )))
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                version: MANIFEST_VERSION,
                ..Self::default()
            }),
            Err(err) => Err(CliError::other(
                anyhow!(err).context(format!("reading {}", path.display())),
            )),
        }
    }

    /// Writes the manifest, making its directory.
    fn write(&self, app_home: &Path) -> Result<(), CliError> {
        let path = manifest_path(app_home);
        let text = serde_json::to_string_pretty(self)
            .map_err(|err| CliError::other(anyhow!(err).context("writing the manifest")))?;
        write_atomic(&path, text.as_bytes())
    }
}

/// Where the manifest is under `app_home`.
fn manifest_path(app_home: &Path) -> PathBuf {
    app_home.join(MANIFEST_DIR).join(MANIFEST_FILE)
}

/// One file the host tree ships, with the bytes to install.
struct SourceFile {
    /// Its path relative to the app home, written with `/` separators.
    rel: String,
    /// Its content.
    bytes: Vec<u8>,
}

impl SourceFile {
    /// The content's hash, as the manifest records it.
    fn hash(&self) -> String {
        sha256(&self.bytes)
    }
}

/// A lowercase hex SHA-256 of `bytes`.
fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// Every file of the host tree that belongs in the app home, sorted by path.
///
/// The tree is `layouts/*.json`, `mods/<name>/...` for every `name` starting with
/// `shprd-`, and `keys.json` — which is read, not copied: its bindings are merged
/// into the person's. `mods/lib/` is the shared modules' source, copied into each
/// mod folder by `bin/sync-shared`, so it is not itself installed.
///
/// Which files there are is decided from the tree's own file list, so a directory
/// and the embedded copy of it install exactly the same set.
fn source_files(source: &Source) -> Result<Vec<SourceFile>, CliError> {
    let mut out = Vec::new();
    for rel in source.walk()? {
        if !installable(&rel) {
            continue;
        }
        out.push(SourceFile {
            bytes: source.read(&rel)?,
            rel,
        });
    }
    out.sort_by(|left, right| left.rel.cmp(&right.rel));
    Ok(out)
}

/// The layouts earlier builds shipped, for one path in the tree.
///
/// A path nothing has replaced has no history, which is every file but a layout.
fn previous_layouts(rel: &str) -> &'static [&'static str] {
    match rel {
        "layouts/shprd.json" => PREVIOUS_LAYOUTS,
        _ => &[],
    }
}

/// Whether `on_disk` is what Herdr left of a layout an earlier build shipped.
///
/// It is the same comparison [`same_minus_terminals`] makes, against the document
/// the build before wrote instead of this one: a file that is structurally the
/// older layout with its terminal blocks pruned is the app's edit, and the shipped
/// file replaces it. A person's own change to that older layout still differs from
/// it, and is still refused.
fn pruned_before(on_disk: &[u8], rel: &str) -> Result<bool, CliError> {
    for previous in previous_layouts(rel) {
        if same_minus_terminals(on_disk, previous.as_bytes())? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether `on_disk` and `shipped` differ only by the terminal blocks Herdr
/// prunes.
///
/// Herdr rewrites a layout file with a terminal block taken out when no terminal
/// of that name exists when the layout loads (measured, `cmd/graph.rs`), because
/// such a block has nothing to show. That rewrite is the app's edit, not the
/// person's, and it is recognised structurally rather than by guessing Herdr's
/// serializer: both documents are parsed, their terminal blocks are removed the
/// way a prune removes them, and the two survivors are compared. A file that
/// differs in anything else — a size, a colour, a key, a block the person added —
/// is the person's and not ours to overwrite.
fn same_minus_terminals(on_disk: &[u8], shipped: &[u8]) -> Result<bool, CliError> {
    let theirs = parse_layout(on_disk)?;
    let ours = parse_layout(shipped)?;
    let mut theirs = without_terminals(theirs);
    let mut ours = without_terminals(ours);
    Ok(theirs == ours || {
        // A prune also takes away what the missing block leaves pointing at
        // nothing (a stack's `active`), so both sides are tidied the same way.
        tidy(&mut theirs);
        tidy(&mut ours);
        theirs == ours
    })
}

/// One layout file as a JSON document.
fn parse_layout(bytes: &[u8]) -> Result<serde_json::Value, CliError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|err| CliError::other(anyhow!(err).context("a layout is not UTF-8")))?;
    serde_json::from_str(text).map_err(|err| {
        CliError::other(anyhow!(err).context("a layout in the host tree is not JSON"))
    })
}

/// `node` with every terminal block removed, and every stack that a removal left
/// empty taken away with it.
fn without_terminals(mut node: serde_json::Value) -> serde_json::Value {
    fn walk(node: &mut serde_json::Value) {
        let Some(object) = node.as_object_mut() else {
            return;
        };
        for key in ["children", "floating"] {
            let Some(array) = object.get_mut(key).and_then(|value| value.as_array_mut()) else {
                continue;
            };
            for child in array.iter_mut() {
                walk(child);
            }
            array.retain(|child| {
                !child
                    .get("content")
                    .and_then(|content| content.as_object())
                    .is_some_and(|content| content.contains_key("terminal"))
                    && !(child
                        .get("children")
                        .and_then(|value| value.as_array())
                        .is_some_and(|children| children.is_empty())
                        && child.get("type").and_then(|kind| kind.as_str()) == Some("stack"))
            });
        }
    }
    walk(&mut node);
    node
}

/// Forgets every `active` that names no child of its node: what a prune leaves.
fn tidy(node: &mut serde_json::Value) {
    let Some(object) = node.as_object_mut() else {
        return;
    };
    for key in ["children", "floating"] {
        if let Some(array) = object.get_mut(key).and_then(|value| value.as_array_mut()) {
            for child in array.iter_mut() {
                tidy(child);
            }
        }
    }
    if let Some(serde_json::Value::String(active)) = object.get("active").cloned() {
        let named = object
            .get("children")
            .and_then(|value| value.as_array())
            .is_some_and(|children| {
                children.iter().any(|child| {
                    child.get("id").and_then(|id| id.as_str()) == Some(active.as_str())
                })
            });
        if !named {
            object.remove("active");
        }
    }
}

/// Whether a path in the tree is one this verb installs.
///
/// A layout is a top-level `layouts/<name>.json`; a mod is every file under
/// `mods/<name>/` for a name this verb owns ([`MOD_PREFIX`]). Everything else —
/// the host folder's `test/`, `bin/`, `README.md`, and the shared modules' source
/// in `mods/lib/` — is not part of what a window needs.
fn installable(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.iter().any(|part| part.starts_with('.')) {
        return false;
    }
    match parts.as_slice() {
        ["layouts", name] => name.ends_with(".json"),
        ["mods", name, ..] if parts.len() > 2 => name.starts_with(MOD_PREFIX),
        _ => false,
    }
}

/// One file's bytes by its path in the tree.
fn read_embedded(rel: &str) -> Result<Vec<u8>, CliError> {
    HOST_FILES
        .iter()
        .find(|(path, _)| *path == rel)
        .map(|(_, bytes)| (*bytes).to_vec())
        .ok_or_else(|| {
            CliError::other(anyhow!(
                "{rel} is not in the desktop embedded in this binary"
            ))
        })
}

/// Every file under `dir`, relative to it, sorted; editor droppings skipped.
fn walk(dir: &Path) -> Result<Vec<String>, CliError> {
    let mut out = Vec::new();
    let mut pending = vec![(dir.to_path_buf(), String::new())];
    while let Some((current, prefix)) = pending.pop() {
        let mut entries = read_dir(&current)?;
        entries.sort();
        for path in entries {
            let name = file_name(&path)?;
            if name.starts_with('.') {
                continue;
            }
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if path.is_dir() {
                pending.push((path, rel));
            } else {
                out.push(rel);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Sorted directory entries.
fn read_dir(dir: &Path) -> Result<Vec<PathBuf>, CliError> {
    let mut entries = Vec::new();
    let read = fs::read_dir(dir).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("reading {}", dir.display())))
    })?;
    for entry in read {
        let entry = entry.map_err(|err| {
            CliError::other(anyhow!(err).context(format!("reading {}", dir.display())))
        })?;
        entries.push(entry.path());
    }
    entries.sort();
    Ok(entries)
}

/// One file's name, as text.
fn file_name(path: &Path) -> Result<String, CliError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .ok_or_else(|| {
            CliError::other(anyhow!(
                "{} is not a name this verb can carry",
                path.display()
            ))
        })
}

/// One file's bytes.
fn read_file(path: &Path) -> Result<Vec<u8>, CliError> {
    fs::read(path)
        .map_err(|err| CliError::other(anyhow!(err).context(format!("reading {}", path.display()))))
}

impl Plan {
    /// Whether this decision writes the file.
    fn writes(self) -> bool {
        matches!(self, Plan::Write | Plan::Restore)
    }
}

/// What install would do with one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    /// Not there yet: write it.
    Write,
    /// There, and already exactly what we ship: nothing to do.
    Same,
    /// There and ours, but Herdr pruned a terminal block out of it: put the
    /// shipped file back.
    Restore,
    /// There, and not ours: refuse the whole install and name it.
    Clash,
}

/// The whole install, decided before anything is written.
struct InstallPlan {
    files: Vec<(SourceFile, Plan)>,
    /// Clashes, as paths relative to the app home.
    clashes: Vec<String>,
    /// The keys to write, `(section, key, command)`.
    keys: Vec<(String, String, String)>,
    /// The keys left alone because the person bound them, `(section, key, theirs)`.
    kept_keys: Vec<(String, String, String)>,
    /// The keys an earlier build wrote that this one no longer ships, given back
    /// because they still hold what was written, `(section, key, command)`.
    retired_keys: Vec<(String, String, String)>,
    /// The merged `keys.json`, when it differs from the one on disk.
    keys_json: Option<Vec<u8>>,
    /// Files an earlier install wrote that this build no longer ships (a mod that
    /// was removed or renamed), still holding exactly what was written: they are
    /// taken away. One the person edited is left alone and is no longer ours.
    stale: Vec<String>,
    /// The directories of those files, which go when they are empty.
    stale_dirs: Vec<String>,
    /// The manifest this install records.
    manifest: Manifest,
}

impl InstallPlan {
    /// Whether the plan would change nothing at all.
    fn is_noop(&self) -> bool {
        self.clashes.is_empty()
            && self.files.iter().all(|(_, plan)| *plan == Plan::Same)
            && self.keys.is_empty()
            && self.retired_keys.is_empty()
            && self.keys_json.is_none()
            && self.stale.is_empty()
    }
}

/// Decides every step of an install against what is on disk now.
///
/// The one rule that puts a file back rather than refusing it: a file this verb
/// wrote, unchanged since, that Herdr rewrote by pruning a layout terminal block
/// it could not resolve. That prune is the app's edit and not the person's — it
/// removes the block and nothing else — so it is recognised and the shipped file
/// is written over it. Every other difference from what we wrote is the person's
/// and is refused.
fn plan(source: &Source, app_home: &Path, previous: &Manifest) -> Result<InstallPlan, CliError> {
    let sources = source_files(source)?;
    let mut files = Vec::with_capacity(sources.len());
    let mut clashes = Vec::new();
    let mut manifest = Manifest {
        version: MANIFEST_VERSION,
        ..Manifest::default()
    };
    for source in sources {
        let target = app_home.join(&source.rel);
        let hash = source.hash();
        // A file we own is one the manifest lists with the hash we last wrote.
        let ours = previous.files.get(&source.rel);
        let decision = match fs::read(&target) {
            Ok(current) => {
                let current_hash = sha256(&current);
                if current_hash == hash {
                    // Already exactly what we ship, whoever wrote it.
                    manifest.files.insert(source.rel.clone(), hash);
                    Plan::Same
                } else if ours == Some(&current_hash) {
                    // A file we wrote, unchanged since: ours to update.
                    manifest.files.insert(source.rel.clone(), hash);
                    Plan::Write
                } else if ours.is_some()
                    && layout_file(&source.rel)
                    && (same_minus_terminals(&current, &source.bytes)?
                        || pruned_before(&current, &source.rel)?)
                {
                    // A file we wrote, then Herdr pruned a terminal block out of:
                    // the app's edit, so the block is put back.
                    manifest.files.insert(source.rel.clone(), hash);
                    Plan::Restore
                } else {
                    clashes.push(source.rel.clone());
                    Plan::Clash
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                manifest.files.insert(source.rel.clone(), hash);
                // Not there at all is the ordinary first install; a file the
                // manifest lists but that is gone was taken away after we wrote
                // it, which is ours to put back.
                if ours.is_some() {
                    Plan::Restore
                } else {
                    Plan::Write
                }
            }
            Err(err) => {
                return Err(CliError::other(
                    anyhow!(err).context(format!("reading {}", target.display())),
                ));
            }
        };
        for dir in parents(&source.rel) {
            // A directory already listed stays listed: uninstall still owns it.
            // `data/` is never claimed, even though the manifest lives under it:
            // Herdr keeps `data/shared.json` there, and a folder the app uses is
            // not one this verb may take back.
            if dir == DATA_DIR {
                continue;
            }
            if previous.dirs.contains(&dir) || !app_home.join(&dir).exists() {
                manifest.dirs.insert(dir);
            }
        }
        files.push((source, decision));
    }

    // The keys are merged after the files are decided, so a clash refuses before
    // any of this is written.
    let ours = source.keys()?;
    let their_path = app_home.join("keys.json");
    let mut theirs = match fs::read_to_string(&their_path) {
        Ok(text) => parse_keys(&text, &their_path)?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(err) => {
            return Err(CliError::other(
                anyhow!(err).context(format!("reading {}", their_path.display())),
            ));
        }
    };
    let before = theirs.clone();
    let mut wrote: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut listed = Vec::new();
    let mut kept = Vec::new();
    for (section, bindings) in &ours {
        for (key, command) in bindings {
            let current = theirs.get(section).and_then(|map| map.get(key));
            let last = previous.keys.get(section).and_then(|map| map.get(key));
            let take = match current {
                None => true,
                // Still what we wrote last time: ours to change.
                Some(value) => last == Some(value),
            };
            if take {
                let changed = current != Some(command);
                theirs
                    .entry(section.clone())
                    .or_default()
                    .insert(key.clone(), command.clone());
                wrote
                    .entry(section.clone())
                    .or_default()
                    .insert(key.clone(), command.clone());
                if changed {
                    listed.push((section.clone(), key.clone(), command.clone()));
                }
            } else {
                // The person's binding stays, and the old record with it, so a
                // later uninstall still gives back what is ours.
                if let Some(previous_command) = last {
                    wrote
                        .entry(section.clone())
                        .or_default()
                        .insert(key.clone(), previous_command.clone());
                }
                kept.push((
                    section.clone(),
                    key.clone(),
                    current.cloned().unwrap_or_default(),
                ));
            }
        }
    }
    // A key an earlier build wrote and this one no longer ships (Ctrl+K moved from
    // `global` to `layout:shprd`): given back if it still holds exactly what we
    // wrote. One the person changed since is theirs and stays.
    let mut retired = Vec::new();
    for (section, bindings) in &previous.keys {
        for (key, command) in bindings {
            if ours.get(section).is_some_and(|map| map.contains_key(key)) {
                continue;
            }
            let holds = theirs.get(section).and_then(|map| map.get(key)) == Some(command);
            if holds {
                if let Some(map) = theirs.get_mut(section) {
                    map.remove(key);
                    if map.is_empty() {
                        theirs.remove(section);
                    }
                }
                retired.push((section.clone(), key.clone(), command.clone()));
            }
        }
    }
    manifest.keys = wrote;
    let keys_json = (theirs != before).then(|| {
        let mut text = serde_json::to_string_pretty(&theirs).unwrap_or_default();
        text.push('\n');
        text.into_bytes()
    });

    // What an earlier install wrote and this build does not ship: taken back, if
    // nobody changed it since.
    let mut stale = Vec::new();
    for (rel, hash) in &previous.files {
        if manifest.files.contains_key(rel) {
            continue;
        }
        if let Ok(current) = fs::read(app_home.join(rel))
            && sha256(&current) == *hash
        {
            stale.push(rel.clone());
        }
    }
    let mut stale_dirs: Vec<String> = previous
        .dirs
        .iter()
        .filter(|dir| !manifest.dirs.contains(*dir))
        .cloned()
        .collect();
    stale_dirs.sort_by_key(|dir| std::cmp::Reverse(dir.matches('/').count()));

    Ok(InstallPlan {
        files,
        clashes,
        stale,
        stale_dirs,
        keys: listed,
        kept_keys: kept,
        retired_keys: retired,
        keys_json,
        manifest,
    })
}

/// An install, refusing every clash before it writes anything.
fn install(source: &Source, app_home: &Path, dry_run: bool, json: bool) -> Result<(), CliError> {
    let previous = Manifest::read(app_home)?;
    let plan = plan(source, app_home, &previous)?;
    if !plan.clashes.is_empty() {
        let mut message = String::from(
            "these files are in the Herdr app home and are not shprd's, so nothing was written:\n",
        );
        for rel in &plan.clashes {
            message.push_str(&format!("  {}\n", app_home.join(rel).display()));
        }
        message.push_str(
            "move them aside (or rename the folder) and run this again: shprd never overwrites \
             a file it did not write",
        );
        return Err(CliError::Invalid(anyhow!(message)));
    }

    if !dry_run {
        for (file, decision) in &plan.files {
            if !decision.writes() {
                continue;
            }
            let target = app_home.join(&file.rel);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|err| {
                    CliError::other(anyhow!(err).context(format!("making {}", parent.display())))
                })?;
            }
            write_atomic(&target, &file.bytes)?;
        }
        for rel in &plan.stale {
            let path = app_home.join(rel);
            fs::remove_file(&path).map_err(|err| {
                CliError::other(anyhow!(err).context(format!("removing {}", path.display())))
            })?;
        }
        // Deepest first; a directory the person filled stays.
        for rel in &plan.stale_dirs {
            let _ = fs::remove_dir(app_home.join(rel));
        }
        if let Some(bytes) = &plan.keys_json {
            write_atomic(&app_home.join("keys.json"), bytes)?;
        }
        plan.manifest.write(app_home)?;
    }

    let changed = plan.files.iter().filter(|(_, plan)| plan.writes()).count();
    if json {
        let value = serde_json::json!({
            "ok": true,
            "dry_run": dry_run,
            "app_home": app_home.display().to_string(),
            "source": source.label(),
            "no_op": plan.is_noop(),
            "written": plan
                .files
                .iter()
                .filter(|(_, plan)| plan.writes())
                .map(|(file, _)| file.rel.clone())
                .collect::<Vec<_>>(),
            "removed": plan.stale.clone(),
            "restored": plan
                .files
                .iter()
                .filter(|(_, plan)| *plan == Plan::Restore)
                .map(|(file, _)| file.rel.clone())
                .collect::<Vec<_>>(),
            "unchanged": plan
                .files
                .iter()
                .filter(|(_, plan)| *plan == Plan::Same)
                .map(|(file, _)| file.rel.clone())
                .collect::<Vec<_>>(),
            "keys_written": plan
                .keys
                .iter()
                .map(|(section, key, command)| serde_json::json!({
                    "section": section,
                    "key": key,
                    "command": command,
                }))
                .collect::<Vec<_>>(),
            "keys_retired": plan
                .retired_keys
                .iter()
                .map(|(section, key, command)| serde_json::json!({
                    "section": section,
                    "key": key,
                    "command": command,
                }))
                .collect::<Vec<_>>(),
            "keys_kept": plan
                .kept_keys
                .iter()
                .map(|(section, key, theirs)| serde_json::json!({
                    "section": section,
                    "key": key,
                    "yours": theirs,
                }))
                .collect::<Vec<_>>(),
        });
        println!("{value}");
        return Ok(());
    }

    let prefix = if dry_run { "would write " } else { "wrote " };
    if plan.is_noop() {
        println!(
            "nothing to do: {} already has this build of the shprd desktop",
            app_home.display()
        );
        return Ok(());
    }
    for (file, decision) in &plan.files {
        if *decision == Plan::Restore {
            println!(
                "{prefix}{} (put back after Herdr pruned it)",
                app_home.join(&file.rel).display()
            );
        } else if *decision == Plan::Write {
            println!("{prefix}{}", app_home.join(&file.rel).display());
        }
    }
    for rel in &plan.stale {
        let word = if dry_run { "would remove" } else { "removed" };
        println!(
            "{word} {} (this build no longer ships it)",
            app_home.join(rel).display()
        );
    }
    for (section, key, command) in &plan.keys {
        println!("{prefix}keys.json {section} {key} = {command}");
    }
    for (section, key, command) in &plan.retired_keys {
        let word = if dry_run { "would remove" } else { "removed" };
        println!("{word} keys.json {section} {key} = {command} (this build binds it elsewhere)");
    }
    for (section, key, theirs) in &plan.kept_keys {
        println!("kept keys.json {section} {key} = {theirs} (bound by you, not changed)");
    }
    if dry_run {
        println!(
            "dry run: nothing was written; {changed} file(s) and {} key(s) would change in {}",
            plan.keys.len() + plan.retired_keys.len(),
            app_home.display()
        );
    } else {
        println!(
            "installed the shprd desktop into {}; pick it in Herdr with ctrl-shift-l",
            app_home.display()
        );
    }
    Ok(())
}

/// An uninstall: take back exactly what the manifest lists.
fn uninstall(app_home: &Path, dry_run: bool, json: bool) -> Result<(), CliError> {
    let present = manifest_path(app_home).exists();
    let manifest = if present {
        Manifest::read(app_home)?
    } else {
        Manifest::default()
    };
    let mut removed = Vec::new();
    let mut kept = Vec::new();
    for (rel, hash) in &manifest.files {
        let path = app_home.join(rel);
        match fs::read(&path) {
            Ok(current) => {
                if sha256(&current) == *hash {
                    removed.push(rel.clone());
                    if !dry_run {
                        fs::remove_file(&path).map_err(|err| {
                            CliError::other(
                                anyhow!(err).context(format!("removing {}", path.display())),
                            )
                        })?;
                    }
                } else {
                    // Edited since we wrote it: the change is the person's, so
                    // the file stays and is no longer ours to take.
                    kept.push(rel.clone());
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(CliError::other(
                    anyhow!(err).context(format!("reading {}", path.display())),
                ));
            }
        }
    }

    // Empty directories go, deepest first; one the person filled stays.
    let mut dirs: Vec<&String> = manifest.dirs.iter().collect();
    dirs.sort_by_key(|dir| std::cmp::Reverse(dir.matches('/').count()));
    let mut removed_dirs = Vec::new();
    for rel in dirs {
        let path = app_home.join(rel);
        let empty = fs::read_dir(&path)
            .map(|mut read| read.next().is_none())
            .unwrap_or(false);
        if empty {
            removed_dirs.push(rel.clone());
            if !dry_run {
                let _ = fs::remove_dir(&path);
            }
        }
    }

    let keys_path = app_home.join("keys.json");
    let mut keys_removed = Vec::new();
    if keys_path.exists() {
        let text = fs::read_to_string(&keys_path).map_err(|err| {
            CliError::other(anyhow!(err).context(format!("reading {}", keys_path.display())))
        })?;
        let mut theirs = parse_keys(&text, &keys_path)?;
        let before = theirs.clone();
        for (section, bindings) in &manifest.keys {
            let Some(map) = theirs.get_mut(section) else {
                continue;
            };
            for (key, command) in bindings {
                if map.get(key) == Some(command) {
                    map.remove(key);
                    keys_removed.push((section.clone(), key.clone()));
                }
            }
            if map.is_empty() {
                theirs.remove(section);
            }
        }
        if theirs != before && !dry_run {
            // An empty `keys.json` says what no `keys.json` says, and this verb
            // wrote its only content: take the file away rather than leave the
            // person an empty one to wonder about.
            if theirs.is_empty() {
                fs::remove_file(&keys_path).map_err(|err| {
                    CliError::other(
                        anyhow!(err).context(format!("removing {}", keys_path.display())),
                    )
                })?;
            } else {
                let mut text = serde_json::to_string_pretty(&theirs)
                    .map_err(|err| CliError::other(anyhow!(err).context("writing keys.json")))?;
                text.push('\n');
                write_atomic(&keys_path, text.as_bytes())?;
            }
        }
    }

    if !dry_run {
        let path = manifest_path(app_home);
        if present {
            fs::remove_file(&path).map_err(|err| {
                CliError::other(anyhow!(err).context(format!("removing {}", path.display())))
            })?;
        }
        // The manifest's own folder goes when it held nothing else, and so does
        // the `data/` this verb made to put it in: an install into a fresh app
        // home leaves no trace of itself behind, and a `data/` the person filled
        // with their own files stays theirs.
        for rel in [MANIFEST_DIR, DATA_DIR] {
            let dir = app_home.join(rel);
            let empty = fs::read_dir(&dir)
                .map(|mut read| read.next().is_none())
                .unwrap_or(false);
            if empty {
                let _ = fs::remove_dir(&dir);
            }
        }
    }

    if json {
        let value = serde_json::json!({
            "ok": true,
            "dry_run": dry_run,
            "app_home": app_home.display().to_string(),
            "nothing_installed": !present,
            "removed": removed,
            "kept": kept,
            "dirs_removed": removed_dirs,
            "keys_removed": keys_removed
                .iter()
                .map(|(section, key)| serde_json::json!({"section": section, "key": key}))
                .collect::<Vec<_>>(),
        });
        println!("{value}");
        return Ok(());
    }

    if !present {
        println!(
            "nothing to remove: {} holds no shprd desktop manifest",
            app_home.display()
        );
        return Ok(());
    }
    let prefix = if dry_run { "would remove " } else { "removed " };
    for rel in &removed {
        println!("{prefix}{}", app_home.join(rel).display());
    }
    for (section, key) in &keys_removed {
        println!("{prefix}keys.json {section} {key}");
    }
    for rel in &kept {
        println!(
            "kept {} (changed since shprd wrote it)",
            app_home.join(rel).display()
        );
    }
    if dry_run {
        println!("dry run: nothing was removed");
    } else {
        println!("removed the shprd desktop from {}", app_home.display());
    }
    Ok(())
}

/// The directories a relative file path needs, outermost first.
fn parents(rel: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut parts = rel.split('/').peekable();
    let mut current = String::new();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            break;
        }
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(part);
        out.push(current.clone());
    }
    out
}

/// Reads a `keys.json` into `section -> key -> command`.
fn read_keys(path: &Path) -> Result<BTreeMap<String, BTreeMap<String, String>>, CliError> {
    match fs::read_to_string(path) {
        Ok(text) => parse_keys(&text, path),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(err) => Err(CliError::other(
            anyhow!(err).context(format!("reading {}", path.display())),
        )),
    }
}

/// Parses a `keys.json`: an object of sections, each of keys to commands.
fn parse_keys(
    text: &str,
    path: &Path,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, CliError> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|err| {
        CliError::other(anyhow!(err).context(format!(
            "{} is not a keys.json (an object of sections)",
            path.display()
        )))
    })?;
    let mut out: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let Some(sections) = value.as_object() else {
        return Err(CliError::other(anyhow!(
            "{} is not a keys.json: it is not an object",
            path.display()
        )));
    };
    for (section, bindings) in sections {
        let Some(bindings) = bindings.as_object() else {
            return Err(CliError::other(anyhow!(
                "{}: section {section:?} is not an object of keys",
                path.display()
            )));
        };
        let mut map = BTreeMap::new();
        for (key, command) in bindings {
            let Some(command) = command.as_str() else {
                return Err(CliError::other(anyhow!(
                    "{}: {section:?} {key:?} is not a command string",
                    path.display()
                )));
            };
            map.insert(key.clone(), command.to_string());
        }
        out.insert(section.clone(), map);
    }
    Ok(out)
}

/// Writes `bytes` to `path` through a temporary file and a rename, so a reader
/// never sees half of one.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    let dir = path
        .parent()
        .ok_or_else(|| CliError::other(anyhow!("{} has no folder", path.display())))?;
    fs::create_dir_all(dir).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("making {}", dir.display())))
    })?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            CliError::other(anyhow!(
                "{} is not a name this verb can write",
                path.display()
            ))
        })?;
    let temporary = dir.join(format!(".{name}.shprd-tmp"));
    fs::write(&temporary, bytes).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("writing {}", temporary.display())))
    })?;
    fs::rename(&temporary, path).map_err(|err| {
        CliError::other(anyhow!(err).context(format!("moving it into {}", path.display())))
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{Manifest, parents, parse_keys, sha256};

    #[test]
    fn a_hash_is_lowercase_hex_of_the_content() {
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn the_directories_a_file_needs_are_outermost_first() {
        assert_eq!(
            parents("mods/shprd-tasks/lib/client.js"),
            vec![
                String::from("mods"),
                String::from("mods/shprd-tasks"),
                String::from("mods/shprd-tasks/lib"),
            ]
        );
        assert_eq!(parents("keys.json"), Vec::<String>::new());
    }

    #[test]
    fn a_keys_file_is_sections_of_commands() {
        let parsed = parse_keys(
            r#"{"global": {"cmd-k": "app.palette"}, "mod:shprd-tasks": {"f": "shprd-tasks.search"}}"#,
            std::path::Path::new("/keys.json"),
        )
        .expect("parsed");
        assert_eq!(parsed["global"]["cmd-k"], "app.palette");
        assert_eq!(parsed["mod:shprd-tasks"]["f"], "shprd-tasks.search");
    }

    #[test]
    fn a_keys_file_that_is_not_one_is_refused() {
        let err = parse_keys("[1, 2]", std::path::Path::new("/keys.json")).expect_err("refused");
        assert!(format!("{err:?}").contains("not an object"));
    }

    #[test]
    fn a_missing_manifest_reads_as_an_empty_one_at_this_version() {
        let dir = tempfile::tempdir().expect("temp dir");
        let manifest = Manifest::read(dir.path()).expect("read");
        assert_eq!(manifest.version, super::MANIFEST_VERSION);
        assert!(manifest.files.is_empty());
        assert!(manifest.dirs.is_empty());
        assert!(manifest.keys.is_empty());
    }
}
