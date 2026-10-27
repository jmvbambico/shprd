//! End-to-end tests for `shprd keys`: every case points `--config` at a temp
//! file, so the real herdr config is never read or written.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// The binary under test, built by cargo for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_shprd");

/// Unwraps a result in a test, failing with the error instead of panicking on
/// the call site.
fn ok<T, E: std::fmt::Debug>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("unexpected failure: {error:?}"),
    }
}

/// A throwaway directory holding one herdr config.
fn temp() -> TempDir {
    ok(TempDir::new())
}

/// Runs the binary with `--config <dir>/config.toml` and the given verb.
fn run(dir: &TempDir, verb: &[&str]) -> (Output, PathBuf) {
    let config = dir.path().join("config.toml");
    let mut all: Vec<String> = vec!["keys".into()];
    all.extend(verb.iter().map(|arg| (*arg).to_string()));
    all.push("--config".into());
    all.push(config.display().to_string());
    match Command::new(BIN).args(&all).output() {
        Ok(output) => (output, config),
        Err(err) => panic!("running {BIN} failed: {err}"),
    }
}

/// The file's text, or the empty string when it does not exist.
fn text(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn keys_show_prints_the_block_and_writes_nothing() {
    let dir = temp();
    let (output, config) = run(&dir, &["show"]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(stdout.starts_with("# >>> shprd (managed) >>>"));
    assert_eq!(stdout.matches("[[keys.command]]").count(), 7);
    assert!(!config.exists(), "show must not create the config");
}

#[test]
fn keys_install_then_uninstall_round_trips_the_file() {
    let dir = temp();
    let config = dir.path().join("config.toml");
    let original = "# mine\n[terminal]\nshell = \"zsh\"\n";
    ok(fs::write(&config, original));
    let (output, _) = run(&dir, &["install"]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.contains("run: herdr server reload-config"),
        "{stdout}"
    );
    let installed = text(&config);
    assert!(installed.starts_with(original), "{installed}");
    assert!(installed.contains("# <<< shprd (managed) <<<"));
    let (output, _) = run(&dir, &["uninstall"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(text(&config), original);
}

#[test]
fn keys_install_exits_two_on_a_conflict() {
    let dir = temp();
    let config = dir.path().join("config.toml");
    let taken = "[[keys.command]]\nkey = \"prefix+alt+u\"\ntype = \"shell\"\ncommand = \"vim\"\n";
    ok(fs::write(&config, taken));
    let (output, _) = run(&dir, &["install"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(text(&config), taken, "a refusal writes nothing");
}

#[test]
fn keys_install_defaults_to_the_env_path_when_config_is_absent() {
    let dir = temp();
    let config = dir.path().join("from-env.toml");
    let output = match Command::new(BIN)
        .args(["keys", "install"])
        .env("HERDR_CONFIG_PATH", &config)
        .output()
    {
        Ok(output) => output,
        Err(err) => panic!("running {BIN} failed: {err}"),
    };
    assert!(output.status.success(), "{output:?}");
    assert!(text(&config).starts_with("# >>> shprd (managed) >>>"));
}
