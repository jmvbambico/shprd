//! End-to-end tests for `shprd roster`: fake CLIs on a private `PATH` and a
//! temp copy of `data/harness`, so no real agent CLI runs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// The shipped `data/harness` directory, copied into each test's tempdir.
fn harness_source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/harness")
        .canonicalize()
        .unwrap_or_else(|_| panic!("the harness data should exist"))
}

/// A temp data directory holding a copy of `data/harness`.
fn data_dir(tmp: &TempDir) -> PathBuf {
    let source = harness_source();
    let harness = tmp.path().join("data/harness");
    if let Err(error) = fs::create_dir_all(&harness) {
        panic!("creating {} failed: {error}", harness.display());
    }
    let Ok(entries) = fs::read_dir(&source) else {
        panic!("reading {} failed", source.display());
    };
    for entry in entries.flatten() {
        let target = harness.join(entry.file_name());
        if let Err(error) = fs::copy(entry.path(), &target) {
            panic!("copying to {} failed: {error}", target.display());
        }
    }
    tmp.path().join("data")
}

/// A shell script standing in for a real agent CLI.
fn write_script(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    if let Err(error) = fs::write(&path, format!("#!/bin/sh\n{body}\n")) {
        panic!("writing {name} failed: {error}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(0o755)) {
            panic!("chmod {name} failed: {error}");
        }
    }
}

/// Runs `shprd roster` with `PATH` limited to `bin` and the copied data dir.
fn run(bin: &Path, data: &Path, args: &[&str]) -> Output {
    match Command::new(BIN)
        .arg("roster")
        .args(args)
        .env("PATH", bin.display().to_string())
        .env("SHPRD_DATA_DIR", data.display().to_string())
        .env_remove("SHPRD_PI_MODEL")
        .output()
    {
        Ok(output) => output,
        Err(error) => panic!("running {BIN} failed: {error}"),
    }
}

#[test]
fn reports_a_fake_cli_and_the_ones_that_are_missing() {
    let tmp = match TempDir::new() {
        Ok(tmp) => tmp,
        Err(error) => panic!("tempdir failed: {error}"),
    };
    let data = data_dir(&tmp);
    let bin = tmp.path().join("bin");
    if let Err(error) = fs::create_dir_all(&bin) {
        panic!("creating {} failed: {error}", bin.display());
    }
    write_script(
        &bin,
        "claude",
        "case \"$1\" in\n  --version) echo '9.9.9 (fake)' ;;\n  auth) exit 0 ;;\nesac",
    );
    let output = run(&bin, &data, &[]);
    assert!(
        output.status.success(),
        "roster failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 5, "one line per CLI: {stdout}");
    let claude = lines
        .iter()
        .find(|line| line.starts_with("claude  "))
        .unwrap_or_else(|| panic!("no claude line in {stdout}"));
    assert!(claude.contains("9.9.9"), "version missing: {claude}");
    assert!(claude.contains("signed in"), "sign-in missing: {claude}");
    assert!(claude.ends_with("anthropic"), "vendor missing: {claude}");
    let codex = lines
        .iter()
        .find(|line| line.starts_with("codex  "))
        .unwrap_or_else(|| panic!("no codex line in {stdout}"));
    assert_eq!(
        *codex,
        "codex  not installed  -  unknown (not installed)  openai"
    );
}

#[test]
fn prints_a_json_array_when_asked() {
    let tmp = match TempDir::new() {
        Ok(tmp) => tmp,
        Err(error) => panic!("tempdir failed: {error}"),
    };
    let data = data_dir(&tmp);
    let bin = tmp.path().join("bin");
    if let Err(error) = fs::create_dir_all(&bin) {
        panic!("creating {} failed: {error}", bin.display());
    }
    write_script(
        &bin,
        "claude",
        "case \"$1\" in\n  --version) echo '9.9.9 (fake)' ;;\n  auth) exit 1 ;;\nesac",
    );
    let output = run(&bin, &data, &["--json"]);
    assert!(
        output.status.success(),
        "roster --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let parsed: serde_json::Value = match serde_json::from_str(&stdout) {
        Ok(value) => value,
        Err(error) => panic!("output is not JSON ({error}): {stdout}"),
    };
    let rows = match parsed.as_array() {
        Some(rows) => rows,
        None => panic!("expected an array, got {parsed}"),
    };
    assert_eq!(rows.len(), 5);
    let claude = match rows.iter().find(|row| row["id"] == "claude") {
        Some(row) => row,
        None => panic!("no claude row in {parsed}"),
    };
    assert_eq!(claude["version"], "9.9.9");
    assert_eq!(claude["signin"], "signed_out");
    assert_eq!(claude["vendor"], "anthropic");
    assert_eq!(claude["reason"], serde_json::Value::Null);
}
