//! Where dagr is, and the command that draws the run's graph.
//!
//! dagr is not on `PATH` on the machine this was built on: it ships inside the
//! `herdr-dagr` herdr plugin, at
//! `~/.config/herdr/plugins/github-installations/herdr-dagr-*/*/checkout/bin/dagr`.
//! So shprd looks in a fixed order — the person's `graph.dagr` setting, then
//! `PATH`, then that plugin folder — and every answer says which probe found it,
//! because "dagr is used from somewhere" is not a thing a person can act on.
//! `$SHPRD_DAGR_BIN` sits in front of all three: it is what a test and a
//! person's own script pin, and it always wins.
//!
//! The choice itself ([`choose`]) is a pure function of what the probes found,
//! so precedence is tested without a filesystem. Reading the filesystem is
//! [`probe`]'s job, and it is handed its answers rather than reading the
//! environment itself: the binary resolves those once at the wiring edge, the
//! same rule the state directory, home and data directory follow.
//!
//! The same location is what `shprd` validates `<repo>/.dagr/run.json` with
//! after every ledger write and what the graph block in the Herdr window runs,
//! so a run's document is checked and drawn by the same binary.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The settings key that names the program.
pub const SETTING: &str = "graph.dagr";

/// The variable that pins the program, in front of every other probe.
pub const ENV: &str = "SHPRD_DAGR_BIN";

/// The program's own name, on `PATH` and in the plugin folder.
pub const NAME: &str = "dagr";

/// The prefix of the plugin folder herdr installs the graph from.
const PLUGIN_PREFIX: &str = "herdr-dagr-";

/// The folder inside a plugin checkout that holds its binaries.
const PLUGIN_BIN: [&str; 3] = ["checkout", "bin", NAME];

/// Which probe named the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `$SHPRD_DAGR_BIN`.
    Env,
    /// The person's `graph.dagr` setting.
    Setting,
    /// A `dagr` on `PATH`.
    Path,
    /// The `herdr-dagr` plugin's checkout.
    Plugin,
}

impl Origin {
    /// The word the API reports.
    pub fn name(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Setting => "setting",
            Self::Path => "path",
            Self::Plugin => "plugin",
        }
    }

    /// The words a diagnostic line uses, naming what to change.
    pub fn label(self) -> &'static str {
        match self {
            Self::Env => "$SHPRD_DAGR_BIN",
            Self::Setting => "graph.dagr",
            Self::Path => "PATH",
            Self::Plugin => "the herdr-dagr plugin",
        }
    }
}

/// A dagr shprd may run, and the probe that named it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    /// The program to run.
    pub path: PathBuf,
    /// Where the answer came from.
    pub origin: Origin,
}

impl Located {
    /// The command that draws `run_json`, as an argument list.
    ///
    /// It is `dagr view <run.json>`, dagr's own follow mode: it watches the file
    /// and redraws when it changes, so the block never has to relaunch it after
    /// a ledger write.
    pub fn command(&self, run_json: &Path) -> Vec<String> {
        vec![
            self.path.display().to_string(),
            "view".to_owned(),
            run_json.display().to_string(),
        ]
    }
}

/// What the probes found, before precedence is applied.
///
/// Every field is "what was there", never "what is usable": the settings and
/// variables are reported as set even when they name nothing, so a person who
/// mistyped a path hears about that path rather than a silent fall-through to
/// `PATH`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probes {
    /// `$SHPRD_DAGR_BIN`, when it is set.
    pub env: Option<PathBuf>,
    /// `graph.dagr`, when it names anything.
    pub setting: Option<PathBuf>,
    /// The first `dagr` on `PATH`.
    pub on_path: Option<PathBuf>,
    /// Every `dagr` under the herdr plugin folder, in path order.
    pub plugins: Vec<PathBuf>,
}

impl Probes {
    /// Whether no probe found anything at all.
    pub fn is_empty(&self) -> bool {
        self.env.is_none()
            && self.setting.is_none()
            && self.on_path.is_none()
            && self.plugins.is_empty()
    }
}

/// Why there is no dagr to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Missing {
    /// No probe found one.
    NotFound,
    /// A probe named something that is not an executable file.
    NotExecutable {
        /// Where the path came from.
        origin: Origin,
        /// The path that probe named.
        path: PathBuf,
    },
}

impl Missing {
    /// One line a person can act on.
    ///
    /// It names the setting, because that is the one answer the person owns: a
    /// `PATH` is the shell's, the plugin is herdr's, and `graph.dagr` is theirs.
    pub fn message(&self) -> String {
        match self {
            Self::NotFound => format!(
                "dagr was not found: set `{SETTING}` to its path, put `{NAME}` on PATH, \
                 or install the herdr-dagr plugin"
            ),
            Self::NotExecutable { origin, path } => format!(
                "`{SETTING}` is not usable: {} names {}, which is not an executable file",
                origin.label(),
                path.display()
            ),
        }
    }
}

impl std::fmt::Display for Missing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for Missing {}

/// The program the probes name, nearest probe first: `$SHPRD_DAGR_BIN`, then
/// `graph.dagr`, then `PATH`, then the herdr plugin.
///
/// A setting or variable that is set wins outright: it is reported as found
/// here even when the path is not there, and [`locate`] is what refuses it. A
/// silent fall-through would make a typo in `graph.dagr` look like a working
/// default.
pub fn choose(probes: &Probes) -> Result<Located, Missing> {
    if let Some(path) = &probes.env {
        return Ok(Located {
            path: path.clone(),
            origin: Origin::Env,
        });
    }
    if let Some(path) = &probes.setting {
        return Ok(Located {
            path: path.clone(),
            origin: Origin::Setting,
        });
    }
    if let Some(path) = &probes.on_path {
        return Ok(Located {
            path: path.clone(),
            origin: Origin::Path,
        });
    }
    if let Some(path) = probes.plugins.first() {
        return Ok(Located {
            path: path.clone(),
            origin: Origin::Plugin,
        });
    }
    Err(Missing::NotFound)
}

/// Gathers what this machine has: the variable, the setting, `PATH` and the
/// herdr plugin folder.
///
/// `config_home` is where herdr keeps its plugins (`$XDG_CONFIG_HOME`, else
/// `<home>/.config`); `path_env` is `PATH` as the caller read it; `env` is
/// `$SHPRD_DAGR_BIN`; `setting` is `graph.dagr`, already resolved from the
/// settings layers. Nothing here reads the environment itself.
pub fn probe(
    config_home: &Path,
    path_env: Option<&OsStr>,
    env: Option<&OsStr>,
    setting: Option<&OsStr>,
) -> Probes {
    Probes {
        env: named(env),
        setting: named(setting),
        on_path: path_env.and_then(first_on_path),
        plugins: plugin_binaries(config_home),
    }
}

/// The dagr this machine has, checked to be a program before it is trusted.
///
/// # Errors
///
/// [`Missing::NotExecutable`] when the probe that won names something that is
/// not an executable file, and [`Missing::NotFound`] when no probe found
/// anything. A `graph.dagr` that names a missing file is an error rather than a
/// fall-through, so a typo cannot be mistaken for the plugin's copy.
pub fn locate(
    config_home: &Path,
    path_env: Option<&OsStr>,
    env: Option<&OsStr>,
    setting: Option<&OsStr>,
) -> Result<Located, Missing> {
    let found = choose(&probe(config_home, path_env, env, setting))?;
    if !is_executable(&found.path) {
        return Err(Missing::NotExecutable {
            origin: found.origin,
            path: found.path,
        });
    }
    Ok(found)
}

/// One of the three values a probe was given, when it names anything.
///
/// An empty setting is unset, which is what the settings model says about every
/// key, so a person who cleared `graph.dagr` gets the fallbacks rather than a
/// path of "".
fn named(value: Option<&OsStr>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// The first `<dir>/dagr` on `PATH` that is an executable file.
fn first_on_path(path_env: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path_env)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(NAME))
        .find(|candidate| is_executable(candidate))
}

/// Every `dagr` the herdr plugin folder holds, sorted so the answer is stable.
///
/// The layout is herdr's: `plugins/github-installations/herdr-dagr-<hash>/<version>/
/// checkout/bin/dagr`. A plugin folder that is not there is no candidates, not
/// an error: most machines have no graph plugin at all.
pub fn plugin_binaries(config_home: &Path) -> Vec<PathBuf> {
    let root = config_home
        .join("herdr")
        .join("plugins")
        .join("github-installations");
    let mut found: Vec<PathBuf> = Vec::new();
    for plugin in sorted_dirs(&root) {
        let name = plugin.file_name().unwrap_or_default().to_string_lossy();
        if !name.starts_with(PLUGIN_PREFIX) {
            continue;
        }
        for version in sorted_dirs(&plugin) {
            let candidate = PLUGIN_BIN
                .iter()
                .fold(version, |path, part| path.join(part));
            if is_executable(&candidate) {
                found.push(candidate);
            }
        }
    }
    found.sort();
    found
}

/// The directories under `dir`, in name order, or nothing when it is not there.
fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect(),
        Err(_) => Vec::new(),
    };
    dirs.sort();
    dirs
}

/// Whether `path` is a file this machine may run.
#[cfg(unix)]
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// Whether `path` is a file this machine may run.
#[cfg(not(unix))]
pub fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

/// The `OsString` a caller's `Path` is handed to [`probe`] as.
pub fn as_os(path: Option<&Path>) -> Option<OsString> {
    path.map(|path| path.as_os_str().to_os_string())
}

/// Where a repository keeps the document the graph draws.
pub fn run_json(repo: &Path) -> PathBuf {
    repo.join(".dagr").join("run.json")
}

/// What the binary found when it looked for dagr, as the service reports it.
///
/// The service does not probe the machine itself: the binary resolves this once
/// at the wiring edge (the same rule the home, prompts and data directories
/// follow) and carries the answer in, so a service started by a test can never
/// find the developer's own plugin folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dagr {
    /// A program the block may run.
    Found(Located),
    /// Why there is none, in words a person can act on.
    Missing(Missing),
}

impl Dagr {
    /// The program, when there is one.
    pub fn located(&self) -> Option<&Located> {
        match self {
            Self::Found(located) => Some(located),
            Self::Missing(_) => None,
        }
    }

    /// The refusal, when there is no program.
    pub fn missing(&self) -> Option<&Missing> {
        match self {
            Self::Found(_) => None,
            Self::Missing(missing) => Some(missing),
        }
    }
}

impl From<Result<Located, Missing>> for Dagr {
    fn from(found: Result<Located, Missing>) -> Self {
        match found {
            Ok(located) => Self::Found(located),
            Err(missing) => Self::Missing(missing),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_setting_is_asked_before_path_and_path_before_the_plugin() {
        // The whole precedence, pure: no filesystem, no environment.
        let all = Probes {
            env: None,
            setting: Some(PathBuf::from("/opt/mine/dagr")),
            on_path: Some(PathBuf::from("/usr/bin/dagr")),
            plugins: vec![PathBuf::from("/plugin/dagr")],
        };
        let found = choose(&all).expect("the setting wins");
        assert_eq!(found.path, PathBuf::from("/opt/mine/dagr"));
        assert_eq!(found.origin, Origin::Setting);

        let path_and_plugin = Probes {
            setting: None,
            ..all.clone()
        };
        let found = choose(&path_and_plugin).expect("PATH wins over the plugin");
        assert_eq!(found.path, PathBuf::from("/usr/bin/dagr"));
        assert_eq!(found.origin, Origin::Path);

        let plugin_only = Probes {
            on_path: None,
            ..path_and_plugin
        };
        let found = choose(&plugin_only).expect("the plugin is the last answer");
        assert_eq!(found.path, PathBuf::from("/plugin/dagr"));
        assert_eq!(found.origin, Origin::Plugin);
    }

    #[test]
    fn the_variable_wins_over_everything() {
        let all = Probes {
            env: Some(PathBuf::from("/pinned/dagr")),
            setting: Some(PathBuf::from("/opt/mine/dagr")),
            on_path: Some(PathBuf::from("/usr/bin/dagr")),
            plugins: vec![PathBuf::from("/plugin/dagr")],
        };
        let found = choose(&all).expect("the variable wins");
        assert_eq!(found.path, PathBuf::from("/pinned/dagr"));
        assert_eq!(found.origin, Origin::Env);
    }

    #[test]
    fn nothing_found_names_the_setting_to_fix() {
        let missing = choose(&Probes::default()).expect_err("nothing is there");
        assert_eq!(missing, Missing::NotFound);
        let message = missing.message();
        assert!(message.contains("graph.dagr"), "{message}");
        assert!(message.contains("herdr-dagr"), "{message}");
    }

    #[test]
    fn the_plugins_are_tried_in_path_order() {
        let probes = Probes {
            plugins: vec![
                PathBuf::from("/a/herdr-dagr-1/1/checkout/bin/dagr"),
                PathBuf::from("/a/herdr-dagr-1/2/checkout/bin/dagr"),
            ],
            ..Probes::default()
        };
        let found = choose(&probes).expect("a plugin is found");
        assert_eq!(
            found.path,
            PathBuf::from("/a/herdr-dagr-1/1/checkout/bin/dagr")
        );
    }

    #[test]
    fn the_command_is_dagrs_own_follow_mode() {
        let found = Located {
            path: PathBuf::from("/opt/dagr"),
            origin: Origin::Setting,
        };
        assert_eq!(
            found.command(Path::new("/repo/.dagr/run.json")),
            vec![
                "/opt/dagr".to_string(),
                "view".to_string(),
                "/repo/.dagr/run.json".to_string()
            ]
        );
    }

    #[test]
    fn a_setting_that_names_nothing_is_unset() {
        // What the settings model does with every empty value, here too: the
        // fallbacks are better than a path of "".
        let probes = probe(Path::new("/nowhere"), None, None, Some(OsStr::new("")));
        assert_eq!(probes.setting, None);
        assert!(probes.is_empty());
    }

    #[test]
    fn the_herdr_plugin_folder_is_found_and_a_missing_one_is_no_error() {
        let home = tempfile::tempdir().expect("config home");
        let version = home
            .path()
            .join("herdr")
            .join("plugins")
            .join("github-installations")
            .join("herdr-dagr-abc123")
            .join("69288-0")
            .join("checkout")
            .join("bin");
        std::fs::create_dir_all(&version).expect("the plugin folder");
        let binary = version.join(NAME);
        std::fs::write(&binary, "#!/bin/sh\nexit 0\n").expect("the binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
                .expect("executable");
        }

        let found = plugin_binaries(home.path());
        assert_eq!(found, vec![binary.clone()], "the plugin's dagr is found");
        let located = locate(home.path(), None, None, None).expect("it is usable");
        assert_eq!(located.path, binary);
        assert_eq!(located.origin, Origin::Plugin);

        let empty = tempfile::tempdir().expect("another config home");
        assert!(plugin_binaries(empty.path()).is_empty());
        assert_eq!(
            locate(empty.path(), None, None, None),
            Err(Missing::NotFound)
        );
    }

    #[test]
    fn a_setting_that_is_not_a_program_is_refused_by_its_own_name() {
        let home = tempfile::tempdir().expect("config home");
        let missing = home.path().join("not-here").join(NAME);
        match locate(home.path(), None, None, Some(missing.as_os_str())) {
            Err(Missing::NotExecutable { origin, path }) => {
                assert_eq!(origin, Origin::Setting);
                assert_eq!(path, missing);
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        let message = Missing::NotExecutable {
            origin: Origin::Setting,
            path: missing,
        }
        .message();
        assert!(message.contains("graph.dagr"), "{message}");
        assert!(message.contains("not an executable file"), "{message}");
    }

    #[test]
    fn a_dagr_on_path_is_found_only_when_it_is_a_program() {
        let dir = tempfile::tempdir().expect("a bin folder");
        let candidate = dir.path().join(NAME);
        std::fs::write(&candidate, "not a program").expect("the file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o644))
                .expect("read only");
        }
        let path_env = dir.path().as_os_str();
        assert_eq!(first_on_path(path_env), None, "a plain file is not dagr");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o755))
                .expect("executable");
            assert_eq!(first_on_path(path_env), Some(candidate));
        }
    }
}
