//! End-to-end tests for `shprd install herdr-desktop`: a temp Herdr app home, a
//! host tree, and the real binary.
//!
//! Every test gets its own app home and its own copy of `host/herdr-desktop`, so
//! nothing here can read or write the developer's own window (`~/.config/herdr-app`)
//! and nothing depends on what that window currently holds.
//!
//! `install` drives no multiplexer, but it is still spawned through the shared
//! isolation rule (`support/herdr.rs`, M10 fix X): a child of a test that runs
//! inside a herdr pane must never see that pane's socket, whatever verb it runs.
//! One rule for every spawn is one rule to keep honest.
//!
//! The acceptance the task asks for, one test each: a fresh install, a re-install
//! that writes nothing, a merge that keeps a key the person bound, an uninstall
//! that removes only the manifest's files, and a dry run that writes nothing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

mod support;

use support::isolate;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// A temp app home and a temp host tree, with the binary run against both.
struct Fixture {
    /// The Herdr app home the install writes into.
    app: TempDir,
    /// A copy of the repository's host tree, so a test can change the "shipped"
    /// files without touching the working copy.
    host: TempDir,
}

impl Fixture {
    /// A fresh app home and a copy of `host/herdr-desktop`.
    fn new() -> Fixture {
        let app = TempDir::new().expect("a temp app home");
        let host = TempDir::new().expect("a temp host tree");
        copy_tree(&host_path(), host.path());
        Fixture { app, host }
    }

    /// A fresh app home with an empty host tree, for a test that never names one.
    fn bare() -> Fixture {
        Fixture {
            app: TempDir::new().expect("a temp app home"),
            host: TempDir::new().expect("a temp host tree"),
        }
    }

    /// Runs `shprd install herdr-desktop` with this fixture's folders.
    fn run(&self, args: &[&str]) -> Output {
        self.command(&["--from", self.host.path().to_str().unwrap()])
            .args(args)
            .output()
            .unwrap_or_else(|error| {
                panic!("running {BIN} failed: {error}");
            })
    }

    /// The binary with nothing but this fixture's app home and XDG folders.
    ///
    /// Every argument after those is the test's: an install that names no tree
    /// is the one that has to find its own, so the command builds only what every
    /// case shares.
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        command
            .args([
                "--home",
                self.app.path().to_str().unwrap(),
                "install",
                "herdr-desktop",
            ])
            .args(
                // The app home first, so an install's own flags come after it.
                ["--app-home", self.app.path().to_str().unwrap()],
            )
            .args(args)
            // The fixture's own XDG directories, and no Herdr home in the
            // environment for the flag to lose to.
            .env("XDG_CONFIG_HOME", self.app.path().join(".config"))
            .env(
                "XDG_STATE_HOME",
                self.app.path().join(".local").join("state"),
            )
            .env_remove("SHPRD_HOST_DIR");
        // The rule first, this fixture's own folders after it: a test spawned
        // from a herdr pane must not inherit the pane's socket, and a live test
        // that wants a private one names it after this call.
        isolate(&mut command, self.app.path());
        command
    }

    /// A path in the app home.
    fn app_path(&self, rel: &str) -> PathBuf {
        self.app.path().join(rel)
    }

    /// The manifest the install wrote, parsed.
    fn manifest(&self) -> Value {
        let text = fs::read_to_string(self.app_path("data/shprd-desktop/manifest.json"))
            .expect("a manifest");
        serde_json::from_str(&text).expect("a JSON manifest")
    }

    /// The manifest's installed files: relative path to hash.
    fn manifest_files(&self) -> BTreeMap<String, String> {
        let value = self.manifest();
        value
            .get("files")
            .and_then(Value::as_object)
            .expect("files")
            .iter()
            .map(|(path, hash)| (path.clone(), hash.as_str().expect("a hash").to_string()))
            .collect()
    }

    /// Every file under the app home, relative, sorted. The manifest's own
    /// file is left out: it is bookkeeping, not an installed one.
    fn files(&self) -> Vec<String> {
        let mut out = Vec::new();
        walk(self.app.path(), self.app.path(), &mut out);
        out.retain(|rel| rel != "data/shprd-desktop/manifest.json");
        out.sort();
        out
    }
}

/// The repository's own host tree.
fn host_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("host")
        .join("herdr-desktop")
}

/// Copies `from` into `to`, recursively.
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("a directory");
    for entry in fs::read_dir(from).expect("the host tree") {
        let entry = entry.expect("an entry");
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            fs::copy(&path, &target).expect("a copied file");
        }
    }
}

/// Every file under `dir`, relative to `root`.
fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out);
        } else {
            let rel = path
                .strip_prefix(root)
                .expect("under the root")
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
}

/// Whether `output` looks like the install succeeded, with its stderr in the
/// panic message when it did not.
fn ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "the command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[test]
fn a_fresh_install_copies_the_layout_and_its_mods() {
    let fixture = Fixture::new();
    ok(&fixture.run(&[]));

    assert!(fixture.app_path("layouts/shprd.json").is_file());
    assert!(fixture.app_path("mods/shprd-tasks/main.js").is_file());
    assert!(fixture.app_path("mods/shprd-tasks/mod.json").is_file());
    // The shared modules are copied into each mod, because a Herdr mod can only
    // import a file inside its own folder.
    assert!(fixture.app_path("mods/shprd-tasks/lib/client.js").is_file());
    assert!(
        fixture
            .app_path("mods/shprd-status/lib/tokens.js")
            .is_file()
    );
    // The shared source folder itself is not a mod, and is not installed.
    assert!(!fixture.app_path("mods/lib").exists());

    // The manifest lists every file it wrote, with the hash on disk.
    let files = fixture.manifest_files();
    assert!(files.contains_key("layouts/shprd.json"));
    assert!(files.contains_key("mods/shprd-tasks/main.js"));
    for (rel, hash) in &files {
        let bytes = fs::read(fixture.app_path(rel)).expect("an installed file");
        assert_eq!(&sha_hex(&bytes), hash, "{rel} hash");
    }
    // And every installed file is in the manifest: the list is the ownership.
    let listed: Vec<String> = files.keys().cloned().collect();
    let mut on_disk = fixture.files();
    on_disk.retain(|rel| rel != "keys.json");
    assert_eq!(on_disk, listed);
}

#[test]
fn a_second_install_writes_nothing() {
    let fixture = Fixture::new();
    ok(&fixture.run(&[]));
    let before = fixture.files();
    let manifest_before = fixture.manifest();
    let keys_before = fs::read(fixture.app_path("keys.json")).expect("keys.json");

    let said = ok(&fixture.run(&[]));
    assert!(
        said.contains("nothing to do"),
        "a re-install should say it did nothing, not: {said}"
    );
    assert_eq!(fixture.files(), before);
    assert_eq!(fixture.manifest(), manifest_before);
    assert_eq!(
        fs::read(fixture.app_path("keys.json")).expect("keys.json"),
        keys_before
    );
}

#[test]
fn a_reinstall_updates_a_file_it_wrote_and_leaves_the_rest() {
    let fixture = Fixture::new();
    ok(&fixture.run(&[]));

    // shprd ships a changed mod in a later build.
    let shipped = fixture.host.path().join("mods/shprd-tasks/main.js");
    let mut text = fs::read_to_string(&shipped).expect("the mod");
    text.push_str("\n// a later build\n");
    fs::write(&shipped, text).expect("the changed mod");

    let before = fixture.manifest_files();
    ok(&fixture.run(&[]));
    let installed = fs::read_to_string(fixture.app_path("mods/shprd-tasks/main.js"))
        .expect("the installed mod");
    assert!(installed.ends_with("// a later build\n"));

    // Only that one file moved: every other entry still names the content it
    // had, and only the changed file's hash is new.
    let after = fixture.manifest_files();
    let changed: Vec<&String> = after
        .iter()
        .filter(|(rel, hash)| before.get(*rel) != Some(*hash))
        .map(|(rel, _)| rel)
        .collect();
    assert_eq!(
        changed,
        vec![&String::from("mods/shprd-tasks/main.js")],
        "only the changed file should be rewritten"
    );
}

#[test]
fn an_install_keeps_a_foreign_file_and_refuses_to_overwrite_it() {
    let fixture = Fixture::new();
    // Something of the person's already sits where shprd's layout goes.
    fs::create_dir_all(fixture.app_path("layouts")).expect("layouts");
    fs::write(fixture.app_path("layouts/shprd.json"), "{\"mine\": true}\n").expect("their layout");

    let output = fixture.run(&[]);
    assert!(!output.status.success(), "a clash must refuse the install");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("layouts/shprd.json"),
        "the refusal should name the file: {stderr}"
    );
    // Nothing at all was written: not the layout, not a mod, not the keys.
    assert_eq!(
        fs::read_to_string(fixture.app_path("layouts/shprd.json")).expect("their layout"),
        "{\"mine\": true}\n"
    );
    assert!(!fixture.app_path("mods/shprd-tasks").exists());
    assert!(!fixture.app_path("keys.json").exists());
    assert!(
        !fixture
            .app_path("data/shprd-desktop/manifest.json")
            .exists()
    );
}

#[test]
fn a_merge_keeps_the_keys_the_person_bound_and_takes_the_rest() {
    let fixture = Fixture::new();
    // The person already has a window with their own bindings, one of which
    // collides with a key shprd's layout would take.
    fs::create_dir_all(fixture.app_path(".")).expect("the app home");
    fs::write(
        fixture.app_path("keys.json"),
        r#"{
  "global": {"cmd-k": "app.palette"},
  "layout:shprd": {"ctrl-shift-b": "space.close"}
}
"#,
    )
    .expect("their keys");

    ok(&fixture.run(&[]));

    let keys: Value = serde_json::from_str(
        &fs::read_to_string(fixture.app_path("keys.json")).expect("keys.json"),
    )
    .expect("JSON keys");
    // Their own section is untouched.
    assert_eq!(keys["global"]["cmd-k"], "app.palette");
    // Their binding of a key shprd wanted is kept.
    assert_eq!(keys["layout:shprd"]["ctrl-shift-b"], "space.close");
    // The key they did not use is taken.
    assert_eq!(
        keys["layout:shprd"]["ctrl-shift-a"],
        "shprd-tabs.show-tasks"
    );
    // And the kept key is not recorded as ours, so uninstall leaves it alone.
    let manifest = fixture.manifest();
    assert!(
        manifest["keys"]["layout:shprd"]
            .get("ctrl-shift-b")
            .is_none(),
        "a kept key must not be recorded as shprd's"
    );
}

#[test]
fn an_m10_install_upgrades_to_the_new_keys_and_keeps_the_person_s_own() {
    let fixture = Fixture::new();
    // What M10 wrote (its two layout keys) beside a key the person bound for
    // themselves: Shepherdr's own Ctrl+K, and Herdr's own F2 left alone.
    fs::create_dir_all(fixture.app_path(".")).expect("the app home");
    fs::write(
        fixture.app_path("keys.json"),
        r#"{
  "global": {"ctrl-k": "mod.open:jump", "f2": "tab.rename"},
  "layout:shprd": {"ctrl-shift-b": "layout.toggle:panel", "ctrl-shift-a": "layout.show:asks"}
}
"#,
    )
    .expect("their keys");

    ok(&fixture.run(&[]));

    let keys: Value = serde_json::from_str(
        &fs::read_to_string(fixture.app_path("keys.json")).expect("keys.json"),
    )
    .expect("JSON keys");
    // The new bindings arrive: the Alt keys globally, Jump and Keybinds inside
    // the shprd layout, the layout's close and new-task keys.
    assert_eq!(keys["layout:shprd"]["ctrl-shift-k"], "mod.open:shprd-jump");
    assert_eq!(
        keys["layout:shprd"]["ctrl-shift-/"],
        "mod.open:shprd-keybinds"
    );
    assert_eq!(keys["global"]["alt-up"], "space.prev");
    assert_eq!(keys["layout:shprd"]["ctrl-shift-w"], "tab.close");
    assert_eq!(keys["layout:shprd"]["ctrl-shift-n"], "shprd-tabs.new-task");
    // Theirs stay: their global Ctrl+K and Herdr's own F2 are never touched; the
    // layout's own Ctrl+K and F2 win inside the shprd layout.
    assert_eq!(keys["layout:shprd"]["ctrl-k"], "mod.open:shprd-jump");
    assert_eq!(keys["layout:shprd"]["f2"], "shprd-tabs.rename-tab");
    assert_eq!(keys["layout:shprd"]["shift-f2"], "shprd-tabs.rename-space");
    assert_eq!(keys["global"]["ctrl-k"], "mod.open:jump");
    assert_eq!(keys["global"]["f2"], "tab.rename");
    // The two mods came with it.
    assert!(fixture.app_path("mods/shprd-jump/main.js").is_file());
    assert!(fixture.app_path("mods/shprd-keybinds/main.js").is_file());
    // A second install is quiet.
    let again = ok(&fixture.run(&["--dry-run"]));
    assert!(!again.is_empty());
}

/// A copy of a real `keys.json` (Herdr's defaults, Shepherdr's section, and the
/// global Ctrl+K / Ctrl+Shift+K / Ctrl+Shift+/ / F2 bindings that made shprd's
/// keys do nothing) with the person's global bindings.
const REAL_KEYS: &str = include_str!("fixtures/keys-herdr-defaults.json");

#[test]
fn the_shprd_keys_land_in_the_layout_and_no_global_key_changes() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.app_path(".")).expect("the app home");
    fs::write(fixture.app_path("keys.json"), REAL_KEYS).expect("their keys");

    let dry = ok(&fixture.run(&["--dry-run"]));
    assert!(
        dry.contains("keys.json layout:shprd ctrl-k = mod.open:shprd-jump"),
        "the dry run shows the layout keys: {dry}"
    );
    assert!(dry.contains("keys.json layout:shprd f2 = shprd-tabs.rename-tab"));
    assert_eq!(
        fs::read_to_string(fixture.app_path("keys.json")).expect("keys.json"),
        REAL_KEYS,
        "a dry run writes nothing"
    );

    ok(&fixture.run(&[]));
    let before: Value = serde_json::from_str(REAL_KEYS).expect("fixture JSON");
    let after: Value = serde_json::from_str(
        &fs::read_to_string(fixture.app_path("keys.json")).expect("keys.json"),
    )
    .expect("JSON keys");
    // Every global key the person had is exactly as it was.
    for (key, command) in before["global"].as_object().expect("global") {
        assert_eq!(after["global"][key], *command, "global {key} changed");
    }
    assert!(after["layout:shprd"]["ctrl-k"] == "mod.open:shprd-jump");
    assert_eq!(after["layout:shprd"]["ctrl-shift-k"], "mod.open:shprd-jump");
    assert_eq!(
        after["layout:shprd"]["ctrl-shift-/"],
        "mod.open:shprd-keybinds"
    );
    assert_eq!(
        after["layout:shprd"]["ctrl-shift-?"],
        "mod.open:shprd-keybinds"
    );
    assert_eq!(after["layout:shprd"]["f2"], "shprd-tabs.rename-tab");
    assert_eq!(after["layout:shprd"]["shift-f2"], "shprd-tabs.rename-space");
    // The sections that are not shprd's are untouched.
    assert_eq!(after["layout:shepherdr"], before["layout:shepherdr"]);
    assert_eq!(after["terminal"], before["terminal"]);
    // Nothing shprd's is left in `global` that shadows nothing: the person's own
    // Ctrl+K stays Herdr's outside this layout.
    assert_eq!(after["global"]["ctrl-k"], "mod.open:jump");
    // A second install is quiet.
    assert!(ok(&fixture.run(&[])).contains("nothing to do"));
}

#[test]
fn a_layout_key_the_person_bound_is_kept_and_reported() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.app_path(".")).expect("the app home");
    fs::write(
        fixture.app_path("keys.json"),
        r#"{"layout:shprd": {"ctrl-k": "app.palette", "f2": "tab.rename"}}"#,
    )
    .expect("their keys");

    let report = ok(&fixture.run(&[]));
    assert!(
        report.contains("kept keys.json layout:shprd ctrl-k = app.palette"),
        "{report}"
    );
    assert!(report.contains("kept keys.json layout:shprd f2 = tab.rename"));
    let json = ok(&fixture.run(&["--dry-run", "--json"]));
    assert!(json.contains("keys_kept"), "{json}");
    let after: Value = serde_json::from_str(
        &fs::read_to_string(fixture.app_path("keys.json")).expect("keys.json"),
    )
    .expect("JSON keys");
    assert_eq!(after["layout:shprd"]["ctrl-k"], "app.palette");
    assert_eq!(after["layout:shprd"]["f2"], "tab.rename");
}

#[test]
fn an_older_install_gives_back_the_global_keys_it_wrote() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.app_path("data/shprd-desktop")).expect("the manifest folder");
    // What the earlier build wrote: Ctrl+K and Ctrl+Shift+K globally; the person
    // has since bound Ctrl+Shift+K to something of their own.
    fs::write(
        fixture.app_path("keys.json"),
        r#"{"global": {"ctrl-k": "mod.open:shprd-jump", "ctrl-shift-k": "app.palette", "alt-1": "tab.select:1"}}"#,
    )
    .expect("their keys");
    fs::write(
        fixture.app_path("data/shprd-desktop/manifest.json"),
        r#"{"version": 1, "files": {}, "dirs": [], "keys": {"global": {"ctrl-k": "mod.open:shprd-jump", "ctrl-shift-k": "mod.open:shprd-jump", "alt-1": "tab.select:1"}}}"#,
    )
    .expect("the old manifest");

    let report = ok(&fixture.run(&[]));
    assert!(
        report.contains("removed keys.json global ctrl-k"),
        "{report}"
    );
    let after: Value = serde_json::from_str(
        &fs::read_to_string(fixture.app_path("keys.json")).expect("keys.json"),
    )
    .expect("JSON keys");
    assert!(after["global"].get("ctrl-k").is_none());
    // The one the person rebound stays.
    assert_eq!(after["global"]["ctrl-shift-k"], "app.palette");
    assert_eq!(after["layout:shprd"]["ctrl-k"], "mod.open:shprd-jump");
}

#[test]
fn an_uninstall_removes_only_what_the_manifest_lists() {
    let fixture = Fixture::new();
    // A file of the person's in the same folders shprd writes to.
    fs::create_dir_all(fixture.app_path("mods/shprd-tasks")).expect("the mod folder");
    fs::write(
        fixture.app_path("mods/shprd-tasks/notes.md"),
        "mine, kept\n",
    )
    .expect("their note");

    ok(&fixture.run(&[]));
    ok(&fixture.run(&["--uninstall"]));

    // Every installed file is gone.
    assert!(!fixture.app_path("layouts/shprd.json").exists());
    assert!(!fixture.app_path("mods/shprd-tasks/main.js").exists());
    assert!(!fixture.app_path("mods/shprd-status").exists());
    assert!(
        !fixture
            .app_path("data/shprd-desktop/manifest.json")
            .exists()
    );
    // Their file is still there, and so is the folder it is in.
    assert_eq!(
        fs::read_to_string(fixture.app_path("mods/shprd-tasks/notes.md")).expect("their note"),
        "mine, kept\n"
    );
    // The keys shprd wrote are gone, and the file with them when nothing is left.
    assert!(!fixture.app_path("keys.json").exists());
}

#[test]
fn an_uninstall_keeps_a_file_the_person_edited() {
    let fixture = Fixture::new();
    ok(&fixture.run(&[]));
    let mine = fixture.app_path("mods/shprd-tasks/main.js");
    fs::write(&mine, "// I changed this\n").expect("their edit");

    let said = ok(&fixture.run(&["--uninstall"]));
    assert!(said.contains("changed since shprd wrote it"), "{said}");
    assert_eq!(
        fs::read_to_string(&mine).expect("their edit"),
        "// I changed this\n"
    );
    // Everything else went.
    assert!(!fixture.app_path("layouts/shprd.json").exists());
}

#[test]
fn an_uninstall_keeps_a_key_the_person_rebound() {
    let fixture = Fixture::new();
    ok(&fixture.run(&[]));
    // The person rebinds one of shprd's keys after the install.
    let mut keys: Value = serde_json::from_str(
        &fs::read_to_string(fixture.app_path("keys.json")).expect("keys.json"),
    )
    .expect("JSON keys");
    keys["layout:shprd"]["ctrl-shift-a"] = Value::String(String::from("space.close"));
    fs::write(
        fixture.app_path("keys.json"),
        serde_json::to_string_pretty(&keys).expect("JSON"),
    )
    .expect("their keys");

    ok(&fixture.run(&["--uninstall"]));
    let after: Value = serde_json::from_str(
        &fs::read_to_string(fixture.app_path("keys.json")).expect("keys.json"),
    )
    .expect("JSON keys");
    assert_eq!(after["layout:shprd"]["ctrl-shift-a"], "space.close");
}

#[test]
fn a_dry_run_writes_nothing() {
    let fixture = Fixture::new();
    let said = ok(&fixture.run(&["--dry-run"]));
    assert!(said.contains("would write"), "{said}");
    assert!(said.contains("dry run"), "{said}");
    assert!(!fixture.app_path("layouts").exists());
    assert!(!fixture.app_path("mods").exists());
    assert!(!fixture.app_path("keys.json").exists());
    assert!(!fixture.app_path("data").exists());

    // And a dry run over an install writes nothing either.
    ok(&fixture.run(&[]));
    let before = fixture.files();
    let manifest_before = fixture.manifest();
    ok(&fixture.run(&["--dry-run"]));
    assert_eq!(fixture.files(), before);
    assert_eq!(fixture.manifest(), manifest_before);
}

#[test]
fn a_dry_run_of_an_uninstall_writes_nothing() {
    let fixture = Fixture::new();
    ok(&fixture.run(&[]));
    let before = fixture.files();
    let said = ok(&fixture.run(&["--uninstall", "--dry-run"]));
    assert!(said.contains("would remove"), "{said}");
    assert_eq!(fixture.files(), before);
    assert!(fixture.app_path("layouts/shprd.json").is_file());
}

#[test]
fn an_uninstall_leaves_the_app_home_as_empty_as_it_found_it() {
    // The whole point of owning only what it wrote: a fresh app home is empty
    // again after the install is taken back, `data/` included. A `data/` left
    // behind is a folder the person has to wonder about.
    let fixture = Fixture::new();
    ok(&fixture.run(&[]));
    assert!(fixture.app_path("data").is_dir());

    ok(&fixture.run(&["--uninstall"]));
    assert_eq!(
        fixture.files(),
        Vec::<String>::new(),
        "nothing of the install is left"
    );
    assert!(
        !fixture.app_path("data").exists(),
        "the empty data/ goes too"
    );
    assert!(!fixture.app_path("mods").exists());
    assert!(!fixture.app_path("layouts").exists());
}

#[test]
fn an_uninstall_keeps_a_data_folder_that_holds_something_else() {
    // Herdr keeps `data/shared.json` in the same folder the manifest lives in,
    // so a `data/` that holds anything else is the app's and the person's.
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.app_path("data")).expect("data");
    fs::write(
        fixture.app_path("data/shared.json"),
        "{\"frame.welcomed\": \"skipped\"}\n",
    )
    .expect("the app's shared state");

    ok(&fixture.run(&[]));
    ok(&fixture.run(&["--uninstall"]));
    assert_eq!(
        fs::read_to_string(fixture.app_path("data/shared.json")).expect("the app's file"),
        "{\"frame.welcomed\": \"skipped\"}\n"
    );
    assert!(fixture.app_path("data").is_dir(), "their data/ stays");
    assert!(
        !fixture.app_path("data/shprd-desktop").exists(),
        "but the manifest's own folder is gone"
    );
}

#[test]
fn an_uninstall_with_nothing_installed_says_so() {
    let fixture = Fixture::new();
    let said = ok(&fixture.run(&["--uninstall"]));
    assert!(said.contains("nothing to remove"), "{said}");
    assert_eq!(fixture.files(), Vec::<String>::new());
}

#[test]
fn the_json_output_carries_the_plan() {
    let fixture = Fixture::new();
    let said = ok(&fixture.run(&["--dry-run", "--json"]));
    let value: Value = serde_json::from_str(said.trim()).expect("a JSON plan");
    assert_eq!(value["ok"], true);
    assert_eq!(value["dry_run"], true);
    let written = value["written"].as_array().expect("written files");
    assert!(written.iter().any(|rel| rel == "layouts/shprd.json"));
    let keys = value["keys_written"].as_array().expect("keys written");
    assert!(keys.iter().any(|key| key["key"] == "ctrl-shift-a"));
}

#[test]
fn a_host_tree_with_no_layouts_is_refused() {
    let fixture = Fixture::new();
    fs::remove_dir_all(fixture.host.path().join("layouts")).expect("no layouts");
    let output = fixture.run(&[]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no `layouts/` directory"), "{stderr}");
    // A tree named outright never falls back to the embedded copy: the person
    // asked for this one.
    assert!(
        stderr.contains("--from"),
        "the refusal names how it was named: {stderr}"
    );
    assert!(!fixture.app_path("mods").exists());
}

// -- the tree this binary carries (the M10 gate's bug 6) ---------------------

#[test]
fn an_install_with_no_host_tree_installs_the_copy_in_the_binary() {
    // A `shprd` with no checkout beside it: no `--from`, no `$SHPRD_HOST_DIR`,
    // and a working directory that is nobody's repository. What it installs is
    // the desktop it was built with, which is the whole point of embedding it.
    let fixture = Fixture::bare();
    let cwd = TempDir::new().expect("a working directory that is nobody's repo");
    let output = fixture
        .command(&[])
        .current_dir(cwd.path())
        .output()
        .expect("the binary runs");
    let said = ok(&output);
    assert!(said.contains("installed the shprd desktop"), "{said}");

    // The same files a checkout installs, byte for byte: the embedded copy is
    // the working tree, not a stale one.
    assert!(fixture.app_path("layouts/shprd.json").is_file());
    assert!(fixture.app_path("mods/shprd-tasks/main.js").is_file());
    assert!(fixture.app_path("mods/shprd-tasks/lib/client.js").is_file());
    assert_eq!(
        fs::read(fixture.app_path("layouts/shprd.json")).expect("the layout"),
        fs::read(host_path().join("layouts/shprd.json")).expect("the shipped layout"),
    );
    assert_eq!(
        fs::read(fixture.app_path("mods/shprd-toasts/main.js")).expect("the mod"),
        fs::read(host_path().join("mods/shprd-toasts/main.js")).expect("the shipped mod"),
    );
    // The host folder's own `test/` and `bin/` are not part of an install.
    assert!(!fixture.app_path("test").exists());
    assert!(!fixture.app_path("bin").exists());
    // And the manifest owns everything the working tree has — no more, no fewer —
    // so an install from the tree beside the checkout and one from the copy in
    // the binary are the same desktop, file for file (a count the tree outgrows
    // is a test that fails for the right reason but says less).
    let from_tree = Fixture::new();
    ok(&from_tree.run(&[]));
    let embedded: Vec<String> = fixture.manifest_files().into_keys().collect();
    let checkout: Vec<String> = from_tree.manifest_files().into_keys().collect();
    assert_eq!(
        embedded, checkout,
        "the embedded copy is the working tree, file for file"
    );
}

#[test]
fn the_working_directory_is_never_the_source_of_the_layout() {
    // The gate's own complaint: run from a worktree, an install found the
    // canonical checkout (or nothing). A tree named outright, the embedded copy,
    // and a worktree's `../host/herdr-desktop` must all produce the same files.
    let from_tree = Fixture::new();
    ok(&from_tree.run(&[]));
    let from_embedded = Fixture::bare();
    let cwd = TempDir::new().expect("a working directory that is nobody's repo");
    let output = from_embedded
        .command(&[])
        .current_dir(cwd.path())
        .output()
        .expect("the binary runs");
    ok(&output);
    assert_eq!(
        from_embedded.files(),
        from_tree.files(),
        "the embedded copy is the working tree"
    );

    // A working directory holding an OLD checkout (an old layout) is not read:
    // the binary installs its own tree, and the old layout stays out.
    let old = TempDir::new().expect("an old checkout");
    copy_tree(&host_path(), &old.path().join("host").join("herdr-desktop"));
    fs::write(
        old.path().join("host/herdr-desktop/layouts/shprd.json"),
        "{\"id\": \"root\", \"type\": \"row\", \"old\": true}\n",
    )
    .expect("old layout");
    fs::create_dir_all(old.path().join("crates")).expect("the crate folder");
    for cwd in [old.path().to_path_buf(), old.path().join("crates")] {
        let from_old_cwd = Fixture::bare();
        let output = from_old_cwd
            .command(&[])
            .current_dir(&cwd)
            .output()
            .expect("the binary runs");
        ok(&output);
        assert_eq!(
            from_old_cwd.files(),
            from_tree.files(),
            "the working directory's tree is not what gets installed"
        );
    }
    // `--repo` still names a checkout outright and wins over the binary's own.
    let by_repo = Fixture::bare();
    let output = by_repo
        .command(&["--repo", old.path().to_str().unwrap()])
        .output()
        .expect("the binary runs");
    ok(&output);
    assert_eq!(
        fs::read_to_string(by_repo.app_path("layouts/shprd.json")).expect("the layout"),
        "{\"id\": \"root\", \"type\": \"row\", \"old\": true}\n"
    );
}

#[test]
fn the_host_dir_variable_wins_over_repo() {
    let fixture = Fixture::new();
    let repo = TempDir::new().expect("a repo");
    copy_tree(
        &host_path(),
        &repo.path().join("host").join("herdr-desktop"),
    );
    fs::write(
        fixture.host.path().join("layouts/shprd.json"),
        "{\"id\": \"root\", \"type\": \"row\", \"env\": true}\n",
    )
    .expect("their layout");
    let output = fixture
        .command(&["--repo", repo.path().to_str().unwrap()])
        .env("SHPRD_HOST_DIR", fixture.host.path())
        .output()
        .expect("the binary runs");
    ok(&output);
    assert_eq!(
        fs::read_to_string(fixture.app_path("layouts/shprd.json")).expect("the layout"),
        "{\"id\": \"root\", \"type\": \"row\", \"env\": true}\n"
    );
}

#[test]
fn an_explicit_from_wins_over_the_embedded_copy() {
    let fixture = Fixture::new();
    // The fixture's tree differs from the shipped one, so what was installed is
    // readable as the fixture's and not as the binary's.
    fs::write(
        fixture.host.path().join("layouts/shprd.json"),
        "{\"id\": \"root\", \"type\": \"row\", \"mine\": true}\n",
    )
    .expect("their layout");
    let output = fixture
        .command(&["--from", fixture.host.path().to_str().unwrap()])
        .output()
        .expect("the binary runs");
    ok(&output);
    assert_eq!(
        fs::read_to_string(fixture.app_path("layouts/shprd.json")).expect("the layout"),
        "{\"id\": \"root\", \"type\": \"row\", \"mine\": true}\n"
    );
}

#[test]
fn a_layout_herdr_pruned_is_put_back() {
    // Herdr resolves a layout terminal block by name when the layout loads and
    // rewrites the file without the block when no such terminal exists. That is
    // the app's edit, not the person's, so the install puts its own file back
    // rather than refusing (the M10 gate's bug 6).
    let fixture = Fixture::new();
    ok(&fixture.run(&[]));
    let installed = fixture.app_path("layouts/shprd.json");
    let shipped = fs::read(&installed).expect("the installed layout");
    let pruned = prune_terminal_blocks(&shipped);
    assert_ne!(pruned, shipped, "the test's prune really removes something");
    fs::write(&installed, &pruned).expect("the pruned layout");

    let said = ok(&fixture.run(&[]));
    assert!(
        said.contains("put back after Herdr pruned it"),
        "the report says what it did: {said}"
    );
    assert_eq!(
        fs::read(&installed).expect("the restored layout"),
        shipped,
        "the shipped file is back, byte for byte"
    );
}

#[test]
fn a_layout_the_person_changed_is_still_refused() {
    // A prune removes the terminal block and nothing else. Anything else a
    // change of the file does is the person's, and is not ours to overwrite.
    let fixture = Fixture::new();
    ok(&fixture.run(&[]));
    let installed = fixture.app_path("layouts/shprd.json");
    let mut layout: Value =
        serde_json::from_slice(&fs::read(&installed).expect("the layout")).expect("JSON");
    // A person's own change on top of the prune: the panel is wider now.
    layout["children"][1]["children"][1]["children"][1]["size"]["px"] = Value::from(560);
    let mine = serde_json::to_vec_pretty(&layout).expect("JSON");
    fs::write(&installed, &mine).expect("their change");

    let output = fixture.run(&[]);
    assert!(!output.status.success(), "a person's change is refused");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("layouts/shprd.json"), "{stderr}");
    assert_eq!(fs::read(&installed).expect("their layout"), mine);
}

/// One of the layouts earlier builds shipped, as those builds wrote it.
///
/// They are the installer's own record of what shprd used to install
/// (`crates/shprd/src/cmd/previous-layouts/`), which is what the upgrade tests need
/// to start from: a desktop a person already has.
fn older_layout(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("cmd")
        .join("previous-layouts")
        .join(name);
    fs::read(&path).unwrap_or_else(|error| panic!("reading {}: {error}", path.display()))
}

/// This build's files with one earlier build's layout, as that install left them.
fn tree_with_layout(name: &str) -> TempDir {
    let tree = TempDir::new().expect("a temp host tree");
    copy_tree(&host_path(), tree.path());
    fs::write(tree.path().join("layouts/shprd.json"), older_layout(name)).expect("the old layout");
    tree
}

/// Installs one tree into a fixture's app home.
fn install_tree(fixture: &Fixture, tree: &TempDir) {
    ok(&fixture
        .command(&["--from", tree.path().to_str().unwrap()])
        .output()
        .expect("the binary runs"));
}

/// Asserts the upgrade landed: this build's layout, on disk and in the manifest,
/// and the side panel keeps no settings block for a tab to switch to.
fn assert_upgraded(fixture: &Fixture) {
    let installed = fs::read(fixture.app_path("layouts/shprd.json")).expect("the layout");
    let shipped = fs::read(host_path().join("layouts/shprd.json")).expect("the shipped layout");
    assert_eq!(installed, shipped, "the layout is replaced, not merged");
    let mut probe: Value = serde_json::from_slice(&installed).expect("JSON");
    assert!(
        find_node(&mut probe, "settings").is_none(),
        "the side panel keeps no settings block"
    );
    let mut probe: Value = serde_json::from_slice(&installed).expect("JSON");
    assert!(
        find_node(&mut probe, "tasks-thread-accept").is_none(),
        "the Accept card is a dialog, not a block of the task thread"
    );
    let mut probe: Value = serde_json::from_slice(&installed).expect("JSON");
    let thread = find_node(&mut probe, "tasks-thread-body").expect("the thread's body");
    assert_eq!(
        thread["active"], "tasks-thread-chat",
        "the thread opens on its chat"
    );
    let mut probe: Value = serde_json::from_slice(&installed).expect("JSON");
    let panel = find_node(&mut probe, "panel-body").expect("the side panel");
    let ids: Vec<&str> = panel["children"]
        .as_array()
        .expect("children")
        .iter()
        .filter_map(|child| child["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["files", "changes", "tasks"],
        "the Asks stack went with the cards moving into the thread, and the graph is a view of the Tasks panel, not a stack of its own"
    );
    let mut probe: Value = serde_json::from_slice(&installed).expect("JSON");
    assert!(
        find_node(&mut probe, "coordinator").is_none(),
        "the sidebar has no coordinator block: its chat is its pane's Chat view"
    );
    let text = String::from_utf8_lossy(&installed);
    assert!(
        !text.contains("\"terminal\"") && !text.contains("graph"),
        "the layout holds no graph block or terminal block for Herdr to prune"
    );
}

#[test]
fn an_install_upgrades_the_desktop_it_wrote_before() {
    // The M10 desktop had settings as a tab of the side panel: a `settings` block
    // in the panel's stack, drawn by the settings mod. M11 fix S makes settings a
    // dialog over the window, so the block is gone from the layout — and a person
    // who already installed the M10 desktop must get the new one by running the
    // install again, not by moving files aside.
    let fixture = Fixture::new();
    install_tree(&fixture, &tree_with_layout("shprd-m10.json"));
    let before = fixture.manifest();
    assert!(
        before["files"]
            .get("layouts/shprd.json")
            .is_some_and(Value::is_string),
        "the M10 install recorded the layout as its own"
    );

    let said = ok(&fixture.run(&[]));
    assert!(
        said.contains("layouts/shprd.json"),
        "the upgrade says it wrote the layout: {said}"
    );
    assert_upgraded(&fixture);

    // Everything else the install owns moved with it, and the manifest now names
    // what is on disk: a second run has nothing left to do.
    let mut hashes = BTreeMap::new();
    for rel in fixture.files() {
        if rel == "keys.json" {
            continue;
        }
        let bytes = fs::read(fixture.app_path(&rel)).expect("an installed file");
        hashes.insert(rel, sha_hex(&bytes));
    }
    assert_eq!(
        fixture.manifest_files(),
        hashes,
        "the manifest names the files the upgrade left"
    );
    let again = ok(&fixture.run(&[]));
    assert!(again.contains("nothing to do"), "{again}");
}

#[test]
fn an_upgrade_puts_back_a_layout_herdr_pruned_from_the_build_before() {
    // The harder half of the same upgrade: the M10 layout a person installed was
    // then pruned by Herdr (the graph terminal did not exist when the window
    // loaded it), so what is on disk is neither the file M10 wrote nor this
    // build's. It is still the app's edit of a layout shprd shipped, so the
    // upgrade replaces it rather than refusing and asking the person to move it.
    let fixture = Fixture::new();
    install_tree(&fixture, &tree_with_layout("shprd-m10.json"));
    let installed = fixture.app_path("layouts/shprd.json");
    let older = fs::read(&installed).expect("the M10 layout");
    fs::write(&installed, prune_terminal_blocks(&older)).expect("the pruned layout");

    let said = ok(&fixture.run(&[]));
    assert!(
        said.contains("put back after Herdr pruned it"),
        "the upgrade says what it did: {said}"
    );
    assert_eq!(
        fs::read(&installed).expect("the upgraded layout"),
        fs::read(host_path().join("layouts/shprd.json")).expect("the shipped layout"),
        "this build's layout is what is left"
    );
}

#[test]
fn an_upgrade_takes_back_a_mod_this_build_no_longer_ships() {
    // M11 U4 removed the Asks panel's and the sidebar coordinator's mods, and
    // SHPR-29 the graph mod (the Graph is a view of the Tasks panel). A desktop
    // installed before that holds them in its manifest, so the upgrade
    // takes them away — unless the person edited one, which is theirs now.
    let fixture = Fixture::new();
    let older = tree_with_layout("shprd-m11-accept.json");
    for gone in ["shprd-asks", "shprd-coordinator", "shprd-graph"] {
        let dir = older.path().join("mods").join(gone);
        fs::create_dir_all(dir.join("lib")).expect("the old mod's folder");
        fs::write(dir.join("main.js"), format!("// {gone}\n")).expect("the old mod");
        fs::write(dir.join("mod.json"), "{\"place\": \"none\"}\n").expect("the old mod.json");
        fs::write(dir.join("lib/tokens.js"), "// a copy\n").expect("the old mod's lib");
    }
    install_tree(&fixture, &older);
    assert!(fixture.app_path("mods/shprd-asks/main.js").is_file());
    let edited = fixture.app_path("mods/shprd-coordinator/main.js");
    fs::write(&edited, "// mine now\n").expect("the person's edit");

    let out = ok(&fixture.run(&[]));
    assert!(
        out.contains("removed"),
        "the upgrade says what it took back: {out}"
    );
    assert!(
        !fixture.app_path("mods/shprd-asks").exists(),
        "an untouched stale mod goes, folder and all"
    );
    assert!(
        !fixture.app_path("mods/shprd-graph").exists(),
        "the graph mod goes with the separate graph panel"
    );
    assert_eq!(
        fs::read_to_string(&edited).expect("their file"),
        "// mine now\n",
        "a file the person edited is left alone"
    );
    assert!(
        fixture.app_path("mods/shprd-chat/main.js").is_file(),
        "the mod that replaced them is there"
    );
    assert_upgraded(&fixture);
    let again = ok(&fixture.run(&[]));
    assert!(
        again.contains("nothing to do"),
        "and it is idempotent: {again}"
    );
}

#[test]
fn a_desktop_from_an_earlier_layout_upgrades_too() {
    // The layout changed twice inside M10 and again in M11, so a person's last
    // install may be the M10.3, M10.4 or M11 (before the Accept dialog) layout
    // rather than M10's last one. Every version is carried,
    // pruned or not, because that is what an upgrade may meet on disk.
    for name in [
        "shprd-m10.3.json",
        "shprd-m10.4.json",
        "shprd-m11.json",
        "shprd-m11-menu.json",
        "shprd-m11-accept.json",
    ] {
        // As the older install left it, and as Herdr leaves it after pruning the
        // terminal block it could not resolve.
        for prune in [false, true] {
            let fixture = Fixture::new();
            install_tree(&fixture, &tree_with_layout(name));
            let installed = fixture.app_path("layouts/shprd.json");
            if prune {
                let older = fs::read(&installed).expect("the older layout");
                fs::write(&installed, prune_terminal_blocks(&older)).expect("the pruned layout");
            }
            ok(&fixture.run(&[]));
            assert_upgraded(&fixture);
            let again = ok(&fixture.run(&[]));
            assert!(
                again.contains("nothing to do"),
                "{name} pruned={prune}: {again}"
            );
        }
    }
}

#[test]
fn an_older_layout_the_person_changed_is_still_refused() {
    // The recognition above is about the app's edit alone: a person who changed
    // the older layout has a file that is not what any build shipped, and it stays
    // exactly as they wrote it.
    let fixture = Fixture::new();
    install_tree(&fixture, &tree_with_layout("shprd-m10.json"));
    let installed = fixture.app_path("layouts/shprd.json");
    let pruned = prune_terminal_blocks(&fs::read(&installed).expect("the M10 layout"));
    let mut document: Value = serde_json::from_slice(&pruned).expect("JSON");
    document["children"][0]["children"][1]["size"]["px"] = Value::from(300);
    let mine = serde_json::to_vec_pretty(&document).expect("JSON");
    fs::write(&installed, &mine).expect("their change");

    let output = fixture.run(&[]);
    assert!(!output.status.success(), "a person's change is refused");
    assert_eq!(fs::read(&installed).expect("their layout"), mine);
}

/// One layout node by id, mutably, or `None`.
fn find_node<'a>(node: &'a mut Value, id: &str) -> Option<&'a mut Value> {
    if node.get("id").and_then(Value::as_str) == Some(id) {
        return Some(node);
    }
    let children = node.get_mut("children")?.as_array_mut()?;
    for child in children.iter_mut() {
        if let Some(found) = find_node(child, id) {
            return Some(found);
        }
    }
    None
}

/// Writes `bytes` back as Herdr does after pruning every terminal block: the
/// block is gone, and so is the `active` of a stack that named it.
///
/// It is the test's own reading of the rule (the installer compares documents
/// structurally), written here so the test does not depend on the installer's
/// idea of what a prune is.
fn prune_terminal_blocks(bytes: &[u8]) -> Vec<u8> {
    fn walk(node: &mut Value) {
        let Some(object) = node.as_object_mut() else {
            return;
        };
        for key in ["children", "floating"] {
            let Some(array) = object.get_mut(key).and_then(Value::as_array_mut) else {
                continue;
            };
            for child in array.iter_mut() {
                walk(child);
            }
            array.retain(|child| {
                !child
                    .get("content")
                    .and_then(Value::as_object)
                    .is_some_and(|content| content.contains_key("terminal"))
            });
        }
        if let Some(active) = object
            .get("active")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            let named = object
                .get("children")
                .and_then(Value::as_array)
                .is_some_and(|children| {
                    children.iter().any(|child| {
                        child.get("id").and_then(Value::as_str) == Some(active.as_str())
                    })
                });
            if !named {
                object.remove("active");
            }
        }
    }
    let mut layout: Value = serde_json::from_slice(bytes).expect("a JSON layout");
    walk(&mut layout);
    let mut out = serde_json::to_vec_pretty(&layout).expect("JSON");
    out.push(b'\n');
    out
}

/// A lowercase hex SHA-256 of `bytes`, the same shape the manifest records.
fn sha_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
