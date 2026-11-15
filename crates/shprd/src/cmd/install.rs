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
//! The source tree is the repository's `host/herdr-desktop/`, resolved at the
//! wiring edge: `$SHPRD_HOST_DIR` first, then `<repo>/host/herdr-desktop`. Every
//! path here is carried in as a value; this module reads no environment variable
//! itself.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cli::InstallArgs;
use crate::ctx::{CliError, resolve_root};

/// Where the manifest lives, relative to the app home.
const MANIFEST_DIR: &str = "data/shprd-desktop";
/// The manifest's file name inside [`MANIFEST_DIR`].
const MANIFEST_FILE: &str = "manifest.json";
/// The version of the manifest's shape.
const MANIFEST_VERSION: u32 = 1;
/// The mod folder prefix this verb owns. A folder named otherwise is Herdr's or
/// the person's, and is never written to or removed.
const MOD_PREFIX: &str = "shprd-";

/// Runs `shprd install`, whichever mode the command line asked for.
pub fn run(args: &InstallArgs, home: Option<&Path>, json: bool) -> Result<(), CliError> {
    let app_home = resolve_app_home(args, home)?;
    if args.uninstall {
        return uninstall(&app_home, args.dry_run, json);
    }
    let host = resolve_host_dir(args)?;
    install(&host, &app_home, args.dry_run, json)
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

/// The host tree to install from: `--from`, then `$SHPRD_HOST_DIR`, then the
/// repository's `host/herdr-desktop`.
fn resolve_host_dir(args: &InstallArgs) -> Result<PathBuf, CliError> {
    let explicit = args.from.clone().or_else(|| {
        std::env::var_os("SHPRD_HOST_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    });
    let candidate = match explicit {
        Some(path) => path,
        None => resolve_root(args.repo.as_deref())
            .map_err(|err| match err {
                CliError::Other(source) => CliError::other(anyhow!(
                    "no host tree: pass --from <dir>, set SHPRD_HOST_DIR, or run this inside the \
                     shprd checkout ({source})"
                )),
                other => other,
            })?
            .join("host")
            .join("herdr-desktop"),
    };
    if !candidate.join("layouts").is_dir() {
        return Err(CliError::other(anyhow!(
            "{} is not a Herdr desktop host tree: it has no `layouts/` directory",
            candidate.display()
        )));
    }
    Ok(candidate)
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
/// The tree is `layouts/*.json`, `mods/<name>/…` for every `name` starting with
/// `shprd-`, and `keys.json` — which is read, not copied: its bindings are merged
/// into the person's. `mods/lib/` is the shared modules' source, copied into each
/// mod folder by `bin/sync-shared`, so it is not itself installed.
fn source_files(host: &Path) -> Result<Vec<SourceFile>, CliError> {
    let mut files = Vec::new();
    let layouts = host.join("layouts");
    let mut entries = read_dir(&layouts)?;
    entries.sort();
    for path in entries {
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let name = file_name(&path)?;
        files.push(SourceFile {
            rel: format!("layouts/{name}"),
            bytes: read_file(&path)?,
        });
    }

    let mods = host.join("mods");
    let mut mod_dirs = read_dir(&mods)?;
    mod_dirs.sort();
    for dir in mod_dirs {
        if !dir.is_dir() {
            continue;
        }
        let name = file_name(&dir)?;
        if !name.starts_with(MOD_PREFIX) {
            continue;
        }
        // A mod's own files, plus the shared modules it carries a copy of.
        for rel in walk(&dir)? {
            files.push(SourceFile {
                rel: format!("mods/{name}/{rel}"),
                bytes: read_file(&dir.join(&rel))?,
            });
        }
    }
    files.sort_by(|left, right| left.rel.cmp(&right.rel));
    Ok(files)
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

/// What install would do with one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    /// Not there yet, or there and byte-identical to what we ship: write it.
    Write,
    /// There, and already exactly what we ship: nothing to do.
    Same,
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
    /// The merged `keys.json`, when it differs from the one on disk.
    keys_json: Option<Vec<u8>>,
    /// The manifest this install records.
    manifest: Manifest,
}

impl InstallPlan {
    /// Whether the plan would change nothing at all.
    fn is_noop(&self) -> bool {
        self.clashes.is_empty()
            && self.files.iter().all(|(_, plan)| *plan == Plan::Same)
            && self.keys.is_empty()
            && self.keys_json.is_none()
    }
}

/// Decides every step of an install against what is on disk now.
fn plan(host: &Path, app_home: &Path, previous: &Manifest) -> Result<InstallPlan, CliError> {
    let sources = source_files(host)?;
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
                } else {
                    clashes.push(source.rel.clone());
                    Plan::Clash
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                manifest.files.insert(source.rel.clone(), hash);
                Plan::Write
            }
            Err(err) => {
                return Err(CliError::other(
                    anyhow!(err).context(format!("reading {}", target.display())),
                ));
            }
        };
        for dir in parents(&source.rel) {
            // A directory already listed stays listed: uninstall still owns it.
            if previous.dirs.contains(&dir) || !app_home.join(&dir).exists() {
                manifest.dirs.insert(dir);
            }
        }
        files.push((source, decision));
    }

    // The keys are merged after the files are decided, so a clash refuses before
    // any of this is written.
    let keys_file = host.join("keys.json");
    let ours = read_keys(&keys_file)?;
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
    manifest.keys = wrote;
    let keys_json = (theirs != before).then(|| {
        let mut text = serde_json::to_string_pretty(&theirs).unwrap_or_default();
        text.push('\n');
        text.into_bytes()
    });

    Ok(InstallPlan {
        files,
        clashes,
        keys: listed,
        kept_keys: kept,
        keys_json,
        manifest,
    })
}

/// An install, refusing every clash before it writes anything.
fn install(host: &Path, app_home: &Path, dry_run: bool, json: bool) -> Result<(), CliError> {
    let previous = Manifest::read(app_home)?;
    let plan = plan(host, app_home, &previous)?;
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
        for (source, decision) in &plan.files {
            if *decision != Plan::Write {
                continue;
            }
            let target = app_home.join(&source.rel);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|err| {
                    CliError::other(anyhow!(err).context(format!("making {}", parent.display())))
                })?;
            }
            write_atomic(&target, &source.bytes)?;
        }
        if let Some(bytes) = &plan.keys_json {
            write_atomic(&app_home.join("keys.json"), bytes)?;
        }
        plan.manifest.write(app_home)?;
    }

    let changed = plan
        .files
        .iter()
        .filter(|(_, plan)| *plan == Plan::Write)
        .count();
    if json {
        let value = serde_json::json!({
            "ok": true,
            "dry_run": dry_run,
            "app_home": app_home.display().to_string(),
            "host": host.display().to_string(),
            "no_op": plan.is_noop(),
            "written": plan
                .files
                .iter()
                .filter(|(_, plan)| *plan == Plan::Write)
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
    for (source, decision) in &plan.files {
        if *decision == Plan::Write {
            println!("{prefix}{}", app_home.join(&source.rel).display());
        }
    }
    for (section, key, command) in &plan.keys {
        println!("{prefix}keys.json {section} {key} = {command}");
    }
    for (section, key, theirs) in &plan.kept_keys {
        println!("kept keys.json {section} {key} = {theirs} (bound by you)");
    }
    if dry_run {
        println!(
            "dry run: nothing was written; {changed} file(s) would change in {}",
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
        // The manifest's own folder goes when it held nothing else.
        let dir = app_home.join(MANIFEST_DIR);
        let empty = fs::read_dir(&dir)
            .map(|mut read| read.next().is_none())
            .unwrap_or(false);
        if empty {
            let _ = fs::remove_dir(&dir);
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
