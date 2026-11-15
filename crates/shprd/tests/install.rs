//! End-to-end tests for `shprd install herdr-desktop`: a temp Herdr app home, a
//! host tree, and the real binary.
//!
//! Every test gets its own app home and its own copy of `host/herdr-desktop`, so
//! nothing here can read or write the developer's own window (`~/.config/herdr-app`)
//! and nothing depends on what that window currently holds.
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

    /// Runs `shprd install herdr-desktop` with this fixture's folders.
    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(BIN);
        command
            .args(["--home", self.app.path().to_str().unwrap(), "install", "herdr-desktop"])
            .args(args)
            .arg("--app-home")
            .arg(self.app.path())
            .arg("--from")
            .arg(self.host.path())
            // The fixture's own XDG directories, and no Herdr home in the
            // environment for the flag to lose to.
            .env("XDG_CONFIG_HOME", self.app.path().join(".config"))
            .env("XDG_STATE_HOME", self.app.path().join(".local").join("state"))
            .env_remove("HERDR_APP_HOME")
            .env_remove("SHPRD_HOST_DIR");
        command.output().unwrap_or_else(|error| {
            panic!("running {BIN} failed: {error}");
        })
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
            .map(|(path, hash)| {
                (
                    path.clone(),
                    hash.as_str().expect("a hash").to_string(),
                )
            })
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
    assert!(fixture.app_path("mods/shprd-status/lib/tokens.js").is_file());
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
    assert!(!fixture.app_path("data/shprd-desktop/manifest.json").exists());
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

    let keys: Value =
        serde_json::from_str(&fs::read_to_string(fixture.app_path("keys.json")).expect("keys.json"))
            .expect("JSON keys");
    // Their own section is untouched.
    assert_eq!(keys["global"]["cmd-k"], "app.palette");
    // Their binding of a key shprd wanted is kept.
    assert_eq!(keys["layout:shprd"]["ctrl-shift-b"], "space.close");
    // The key they did not use is taken.
    assert_eq!(keys["layout:shprd"]["ctrl-shift-a"], "layout.show:asks");
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
    assert!(!fixture.app_path("data/shprd-desktop/manifest.json").exists());
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
    assert!(!fixture.app_path("mods").exists());
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
