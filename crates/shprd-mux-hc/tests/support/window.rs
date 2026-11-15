//! A stand-in for the Herdr window's `herdr-app` command line.
//!
//! [`shprd_mux_hc::HcHost`] drives two programs: herdr-core for terminals, and
//! the window for the layout. [`tests/support/fake_hc.py`](../fake_hc.py) is the
//! first; this is the second — a shell script that answers the commands the host
//! sends with the shapes M7ef §2.12 recorded, and writes down every invocation
//! so a test can prove what the host asked the window to do. Nothing here needs
//! a window, a session or the person's desktop.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

/// A stand-in for the window's `herdr-app` command line.
///
/// It answers the commands [`shprd_mux_hc::HcHost`] sends — `status`, and the
/// `do …` calls that change the layout — with the shapes M7ef §2.12 recorded,
/// and writes every invocation to a log the test reads. A window command the
/// test has not scripted answers `{"ok":true}`, so a verb that only needs the
/// window to accept something can be tested too.
#[derive(Debug)]
pub struct Window {
    dir: std::sync::Arc<tempfile::TempDir>,
}

impl Window {
    /// A window whose calls are logged in a folder of its own.
    pub fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("shprd-fake-window-")
            .tempdir()
            .expect("a temporary folder for the fake window");
        let window = Self {
            dir: std::sync::Arc::new(dir),
        };
        window.write_script();
        window
    }

    /// The `herdr-app` to hand [`shprd_mux_hc::Window::with_binary`].
    pub fn binary(&self) -> String {
        self.path("herdr-app").to_string_lossy().into_owned()
    }

    /// A window value pointed at this script.
    pub fn window(&self) -> shprd_mux_hc::Window {
        shprd_mux_hc::Window::with_binary(self.binary())
    }

    /// Every command line the window was run with, in order.
    pub fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// One path inside the window's folder.
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Writes the script, with its log path baked in.
    fn write_script(&self) {
        let log = self.path("calls").to_string_lossy().into_owned();
        let script = format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{log}'
if [ "$1" = "status" ]; then
  echo '{{"layout":"main","space":"spc3f2a"}}'
  exit 0
fi
case "$2" in
  space.new)  echo '{{"space":"spcnew","tab":"tbnew","terminal":"t100"}}' ;;
  pane.split) echo '{{"pane":"pnew"}}' ;;
  *)          echo '{{"ok":true}}' ;;
esac
"#
        );
        std::fs::write(self.path("herdr-app"), script).expect("writes the window script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(self.path("herdr-app"))
                .expect("the script exists")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(self.path("herdr-app"), permissions)
                .expect("makes the script executable");
        }
    }
}

impl Default for Window {
    fn default() -> Self {
        Self::new()
    }
}
