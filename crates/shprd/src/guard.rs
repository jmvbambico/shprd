//! The one thing `shprd serve` will not do to a machine that is not its own.
//!
//! # Why this exists
//!
//! `shprd serve` prepares the repository's own space at startup (M9 fix J), and
//! it reaches that space through whatever herdr its environment names. A worker
//! inside a herdr pane inherits that pane's `HERDR_SOCKET_PATH` — the person's
//! own session — so a live check run from a checkout under `$TMPDIR` created a
//! workspace labelled `shprd · repo` in the person's window. It happened twice
//! in one day: SHP-66's live login check, then SHP-70's screenshot run, both
//! from `<tmp>/SHP-*/repo`.
//!
//! The tests were already told to isolate themselves (`isolate` in
//! `crates/shprd-mux-herdr/tests/support/herdr_env.rs`, M10 fix X), but a rule
//! that only tests follow cannot help the `shprd serve` a person or a worker
//! starts by hand for a screenshot. So the binary keeps the rule as well, as a
//! belt-and-braces: it is cheaper to refuse to make a space than to close one in
//! somebody's window.
//!
//! # The rule, exactly
//!
//! `shprd serve` refuses to create the repository's space — and refuses to start
//! the graph block's terminal, which is the other thing it would put in the
//! person's host — when **both** hold:
//!
//! 1. the repository root is inside the system temporary directory
//!    (`std::env::temp_dir()`), and
//! 2. the herdr socket the service would drive is **not** inside that same
//!    directory: `HERDR_SOCKET_PATH` is unset (so `herdr` resolves the person's
//!    own socket, `$XDG_CONFIG_HOME/herdr/herdr.sock` or
//!    `~/.config/herdr/herdr.sock`), or it names a path outside the temporary
//!    directory.
//!
//! The two halves are what make it a leak rather than a guess. A checkout under
//! the temporary directory is a throwaway — a live check, a screenshot, a test
//! fixture — and a socket outside it is a session somebody is working in. Either
//! half alone is an ordinary thing: a person may keep a real checkout in `/tmp`,
//! and a service in a real checkout drives their own session on purpose.
//!
//! # Why not "the socket is the default one"
//!
//! Comparing the socket against the path `herdr` would resolve by default looks
//! like the same test and is not: a worker that inherits `HERDR_SOCKET_PATH` but
//! exports its own `XDG_CONFIG_HOME` (which every live fixture does) has a
//! socket that is the person's and a default that is not, so the comparison says
//! "different" about the one case it exists for. "Outside the temporary
//! directory the repository lives in" needs no knowledge of where the person's
//! socket is, and a live check that does the right thing — a socket of its own
//! under its own temporary folder, which is what [`isolate`] gives every test —
//! is never refused.
//!
//! # Getting out of it
//!
//! `SHPRD_ALLOW_DEFAULT_SOCKET=1` turns the guard off, because a person whose
//! own checkout really does live in `/tmp` must not lose their service to a rule
//! written for workers. The refusal names the variable, so nobody has to read
//! this file to get unstuck.
//!
//! [`isolate`]: ../../shprd_mux_herdr/tests/support/herdr_env.rs

use std::path::{Path, PathBuf};

/// The variable that turns the guard off, for a person whose own checkout is
/// under the temporary directory and who means their own session.
pub const ALLOW_ENV: &str = "SHPRD_ALLOW_DEFAULT_SOCKET";

/// The variable naming the herdr socket, as [`crate::host`] reads it.
const SOCKET_ENV: &str = "HERDR_SOCKET_PATH";

/// What the guard compares, resolved once at the wiring edge.
///
/// The environment is read in [`Edge::from_environment`] and nowhere else, so
/// the decision below is a pure function of these four values and a test can
/// name the machine it wants to ask about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    /// The repository the service was pointed at.
    pub repo: PathBuf,
    /// The system temporary directory, as `std::env::temp_dir()` resolved it.
    pub temp: PathBuf,
    /// `HERDR_SOCKET_PATH`, when the environment sets one.
    pub socket: Option<PathBuf>,
    /// Whether `SHPRD_ALLOW_DEFAULT_SOCKET` says the person means it.
    pub allowed: bool,
}

impl Edge {
    /// The edge this process is on, for `repo`.
    pub fn from_environment(repo: &Path) -> Self {
        let socket = std::env::var_os(SOCKET_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        Self {
            repo: repo.to_path_buf(),
            temp: std::env::temp_dir(),
            socket,
            allowed: std::env::var_os(ALLOW_ENV).is_some_and(|value| value == "1"),
        }
    }
}

/// Why the service must not touch the host, or `None` when it may.
///
/// A `Some` is the whole message: it is printed in the banner and logged, and it
/// names both paths and both ways out, so a person reading one line knows what
/// happened and what to do.
pub fn leakage(edge: &Edge) -> Option<String> {
    if edge.allowed {
        return None;
    }
    if !contains(&edge.temp, &edge.repo) {
        return None;
    }
    // A socket of the checkout's own is a private server, and driving it is the
    // whole point of a live check.
    if edge
        .socket
        .as_deref()
        .is_some_and(|socket| contains(&edge.temp, socket))
    {
        return None;
    }
    let socket = match &edge.socket {
        Some(socket) => format!("{} is not", socket.display()),
        None => format!("{SOCKET_ENV} is unset, so herdr resolves the person's own"),
    };
    Some(format!(
        "refused to create this repository's space: {} is inside the temporary directory {}, \
         and {socket}. A live check run from a temporary checkout would create a workspace in \
         the person's own herdr, so nothing was created and no terminal was started. Point \
         {SOCKET_ENV} at a socket inside {} (host/herdr-desktop/bin/isolate does that for a \
         whole command), or set {ALLOW_ENV}=1 if you really mean the person's own session",
        edge.repo.display(),
        edge.temp.display(),
        edge.temp.display(),
    ))
}

/// Whether `child` is `parent` or under it.
///
/// Both spellings are tried, as given and with the longest existing prefix
/// canonicalised: `/var` is a symlink to `/private/var` on macOS, so `TMPDIR`
/// and a freshly created temporary folder can disagree about the prefix while
/// naming the same directory.
fn contains(parent: &Path, child: &Path) -> bool {
    if child.starts_with(parent) {
        return true;
    }
    match (canonical(parent), canonical(child)) {
        (Some(parent), Some(child)) => child.starts_with(parent),
        _ => false,
    }
}

/// `path` with its longest existing prefix canonicalised.
///
/// A path that does not exist yet — the socket of a server nobody has started —
/// has no canonical form, so its parent is canonicalised and the rest is
/// appended. That is enough for a comparison of directories, and it never fails
/// the guard open: a path that cannot be resolved is compared as it was given,
/// which for a socket outside the temporary directory is already the answer.
fn canonical(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = path.canonicalize() {
        return Some(canonical);
    }
    let name = path.file_name()?;
    let parent = canonical(path.parent()?)?;
    Some(parent.join(name))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::{Path, PathBuf};

    use super::{ALLOW_ENV, Edge, leakage};

    /// A machine with the temporary directory at `/tmp`, a throwaway checkout
    /// under it, and no socket named at all: the shape of the leak.
    fn leak() -> Edge {
        Edge {
            repo: PathBuf::from("/tmp/SHP-66/live/repo"),
            temp: PathBuf::from("/tmp"),
            socket: None,
            allowed: false,
        }
    }

    #[test]
    fn a_temporary_checkout_with_no_socket_named_is_refused() {
        let reason = leakage(&leak()).expect("the guard fires");
        assert!(
            reason.starts_with("refused to create this repository's space"),
            "it says what it did not do: {reason}"
        );
        assert!(reason.contains("/tmp/SHP-66/live/repo"), "{reason}");
        assert!(
            reason.contains("HERDR_SOCKET_PATH is unset"),
            "and why: {reason}"
        );
        assert!(reason.contains(ALLOW_ENV), "and the way out: {reason}");
    }

    #[test]
    fn a_temporary_checkout_with_the_persons_socket_named_is_refused() {
        // The incident: the pane's socket was inherited, and it is not the
        // checkout's.
        let edge = Edge {
            socket: Some(PathBuf::from("/Users/someone/.config/herdr/herdr.sock")),
            ..leak()
        };
        let reason = leakage(&edge).expect("the guard fires");
        assert!(
            reason.contains("/Users/someone/.config/herdr/herdr.sock is not"),
            "the socket it would have driven is named: {reason}"
        );
    }

    #[test]
    fn a_temporary_checkout_with_a_socket_of_its_own_is_left_alone() {
        // What `isolate` gives every test, and what a live check should do: the
        // server is the checkout's, so making a workspace in it harms nobody.
        let edge = Edge {
            socket: Some(PathBuf::from("/tmp/SHP-66/live/state/no-herdr-server.sock")),
            ..leak()
        };
        assert_eq!(leakage(&edge), None);
    }

    #[test]
    fn a_real_checkout_may_drive_the_persons_own_session() {
        // Not a leak, and not the guard's business: this is a person running
        // their own service against the herdr they are working in.
        let edge = Edge {
            repo: PathBuf::from("/Users/someone/projects/shprd"),
            socket: Some(PathBuf::from("/Users/someone/.config/herdr/herdr.sock")),
            ..leak()
        };
        assert_eq!(leakage(&edge), None);
    }

    #[test]
    fn the_variable_that_means_it_turns_the_guard_off() {
        let edge = Edge {
            allowed: true,
            ..leak()
        };
        assert_eq!(
            leakage(&edge),
            None,
            "a person whose own checkout is under /tmp keeps their service"
        );
    }

    #[test]
    fn a_sibling_whose_name_only_starts_the_same_is_not_inside() {
        // `/tmp-other` is not `/tmp`: a prefix comparison would say it was, and
        // then a checkout under it would be refused for no reason.
        let edge = Edge {
            repo: PathBuf::from("/tmp-other/repo"),
            ..leak()
        };
        assert_eq!(leakage(&edge), None);
    }

    #[test]
    fn a_socket_beside_the_temporary_directory_is_not_inside_it() {
        let edge = Edge {
            socket: Some(PathBuf::from("/tmp-other/herdr.sock")),
            ..leak()
        };
        assert!(leakage(&edge).is_some(), "outside is outside");
    }

    #[test]
    fn a_real_path_is_compared_with_its_symlinks_resolved() {
        // The macOS spelling of the leak: `TMPDIR` says `/var/folders/…` and the
        // folder `tempfile` made resolves under `/private/var/folders/…`.
        let temp = tempfile::tempdir().unwrap_or_else(|err| panic!("{err}"));
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap_or_else(|err| panic!("{err}"));
        let edge = Edge {
            repo: repo.clone(),
            temp: temp.path().to_path_buf(),
            socket: None,
            allowed: false,
        };
        assert!(
            leakage(&edge).is_some(),
            "a real temporary folder holds its checkout, however it is spelled"
        );
        let spelled = Edge {
            repo: Path::new(&format!(
                "{}",
                temp.path().canonicalize().unwrap().display()
            ))
            .join("repo"),
            temp: temp.path().to_path_buf(),
            ..edge
        };
        assert!(
            leakage(&spelled).is_some(),
            "and resolving one side and not the other changes nothing"
        );
    }
}
