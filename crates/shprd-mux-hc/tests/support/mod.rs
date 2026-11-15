//! Spawning the fake `hc connect` and reading what it did.
//!
//! The fake is [`tests/support/fake_hc.py`](fake_hc.py): a real child process on
//! a real pipe, speaking the bytes M7ef recorded. Each fake gets its own
//! directory under the test's temporary folder, which is where its mode, its
//! spawn marker and its request log live — never a home directory, never the
//! person's own Herdr desktop.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

#[path = "live.rs"]
pub mod live;

use std::path::{Path, PathBuf};

/// One fake `hc connect`, with its own script directory.
///
/// The folder is deleted when the last handle goes, which also ends the child:
/// its stdin closes with the test.
#[derive(Debug)]
pub struct Fake {
    dir: std::sync::Arc<tempfile::TempDir>,
}

impl Clone for Fake {
    fn clone(&self) -> Self {
        Self {
            dir: std::sync::Arc::clone(&self.dir),
        }
    }
}

impl Fake {
    /// A fake whose first line of behaviour is `mode`.
    ///
    /// The mode names what [`fake_hc.py`](fake_hc.py) does: `normal`, `old`,
    /// `gen2`, `quiet`, `die-once`, `missed` or `doc`.
    pub fn new(mode: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("shprd-mux-hc-")
            .tempdir()
            .expect("a temporary folder for the fake");
        let fake = Self {
            dir: std::sync::Arc::new(dir),
        };
        std::fs::write(fake.dir.path().join("mode"), mode).expect("writes the mode");
        fake
    }

    /// The folder the fake keeps its mode, marker and log in.
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// The path to pass as the `hc` binary: a tiny shell script that runs the
    /// Python fake with this fake's folder in `FAKE_HC_DIR`.
    ///
    /// A shell wrapper rather than `python3` directly, because the client spawns
    /// the binary with `connect` as its only argument and a real `hc` takes it.
    pub fn binary(&self) -> String {
        let wrapper = self.dir.path().join("hc");
        let script = self.dir.path().join("fake_hc.py");
        std::fs::write(&script, include_str!("fake_hc.py")).expect("writes the fake");
        let text = format!(
            "#!/bin/sh\nFAKE_HC_DIR='{}' exec python3 '{}' \"$@\"\n",
            self.dir.path().display(),
            script.display()
        );
        std::fs::write(&wrapper, text).expect("writes the wrapper");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(&wrapper)
                .expect("the wrapper exists")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&wrapper, permissions).expect("makes it executable");
        }
        wrapper.to_string_lossy().into_owned()
    }

    /// Every method the fake was asked, in order.
    pub fn requests(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// How many children were spawned.
    pub fn spawns(&self) -> u32 {
        std::fs::read_to_string(self.path("child-marker"))
            .unwrap_or_default()
            .trim()
            .parse()
            .unwrap_or_default()
    }

    /// Whether the fake's last child has ended.
    ///
    /// The fake writes a line when it starts and one when it stops, so a test
    /// can tell "killed" from "still there" without reading the process table.
    pub fn children_finished(&self) -> bool {
        let events = self.events();
        events
            .last()
            .is_some_and(|last| last.starts_with("stopped"))
    }

    /// Every start and stop the fake recorded.
    pub fn events(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("life"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Whether the fake is still running, by its spawn marker only: the fake
    /// exits when its stdin ends, so a test must not assume it lives.
    pub fn exists(&self) -> bool {
        self.dir().exists()
    }

    /// One path inside the fake's folder.
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fake_has_its_own_folder_and_a_runnable_binary() {
        let fake = Fake::new("normal");
        let binary = fake.binary();
        assert!(std::path::Path::new(&binary).is_file(), "{binary}");
        assert_eq!(fake.spawns(), 0);
        assert!(fake.requests().is_empty());
        assert!(fake.exists());
    }
}
