//! `tasks.news_settle_secs` is read on every pass, not once at start-up.

use shprd_server::tick::settle_override_ms;

/// Writes the person's settings file into `home`.
fn write_settings(home: &std::path::Path, text: &str) {
    let dir = home.join(".config").join("shprd");
    std::fs::create_dir_all(&dir).unwrap_or_else(|err| panic!("{err}"));
    std::fs::write(dir.join("settings.toml"), text).unwrap_or_else(|err| panic!("{err}"));
}

#[test]
fn a_saved_news_settle_is_the_next_passes_window() {
    let home = tempfile::tempdir().unwrap_or_else(|err| panic!("{err}"));
    let repo = tempfile::tempdir().unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(
        settle_override_ms(home.path(), repo.path()),
        None,
        "unset, the desk keeps the window it was wired with"
    );
    write_settings(home.path(), "version = 1\n[tasks]\nnews_settle_secs = 12\n");
    assert_eq!(settle_override_ms(home.path(), repo.path()), Some(12_000));
    // Saved again while the service runs: the very next read sees it.
    write_settings(home.path(), "version = 1\n[tasks]\nnews_settle_secs = 0\n");
    assert_eq!(settle_override_ms(home.path(), repo.path()), Some(0));
}
