//! The few file and secret helpers every module here needs.
//!
//! They are deliberately small and local: the service's own private-file code
//! lives in `shprd-server`, which depends on this crate, so borrowing it would
//! point the dependency the wrong way.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use subtle::ConstantTimeEq;

use crate::error::RemoteError;

/// Compares two secrets without letting the time say how much of one was right.
///
/// `subtle`'s implementation is the one the researched design names, and it is
/// used for every secret in this crate: the bearer token, a session id against
/// its stored hash, and a password's digest against the stored one.
pub fn secret_eq(left: &[u8], right: &[u8]) -> bool {
    left.ct_eq(right).into()
}

/// Creates `dir` and makes it private (`0700`).
pub(crate) fn create_private_dir(dir: &Path) -> Result<(), RemoteError> {
    fs::create_dir_all(dir).map_err(|err| RemoteError::io(dir, err))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::Permissions::from_mode(0o700);
        fs::set_permissions(dir, mode).map_err(|err| RemoteError::io(dir, err))?;
    }
    Ok(())
}

/// Writes `bytes` to `path` through a temporary file in the same directory.
///
/// A reader either sees the old file or the new one, never a half-written
/// one — which matters for `sessions.json`, read by a running service while
/// the CLI revokes a session in another process.
pub(crate) fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), RemoteError> {
    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }
    let temporary = path.with_extension("tmp");
    write_private(&temporary, bytes)?;
    fs::rename(&temporary, path).map_err(|err| RemoteError::io(path, err))?;
    Ok(())
}

/// Writes `bytes` to `path`, creating it private (`0600`) and truncating it.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), RemoteError> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|err| RemoteError::io(path, err))?;
    file.write_all(bytes)
        .map_err(|err| RemoteError::io(path, err))?;
    file.sync_all().map_err(|err| RemoteError::io(path, err))?;
    set_private(path)
}

/// Re-asserts a file's private mode (`0600`).
pub(crate) fn set_private(path: &Path) -> Result<(), RemoteError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, mode).map_err(|err| RemoteError::io(path, err))?;
    }
    let _ = path;
    Ok(())
}

/// `bytes` bytes of randomness.
///
/// `/dev/urandom` is the one source every unix has; it is the same source the
/// service draws its bearer token from, so there is one way a secret is made
/// here rather than two.
pub(crate) fn random_bytes(buffer: &mut [u8]) -> Result<(), RemoteError> {
    let mut source = fs::File::open("/dev/urandom").map_err(|err| RemoteError::Secret {
        reason: err.to_string(),
    })?;
    source
        .read_exact(buffer)
        .map_err(|err| RemoteError::Secret {
            reason: err.to_string(),
        })?;
    Ok(())
}

/// `bytes` bytes of randomness, as lowercase hex.
pub(crate) fn random_hex(bytes: usize) -> Result<String, RemoteError> {
    let mut buffer = vec![0u8; bytes];
    random_bytes(&mut buffer)?;
    Ok(hex(&buffer))
}

/// `bytes` as lowercase hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_secret_compares_exactly() {
        assert!(secret_eq(b"abc", b"abc"));
        assert!(!secret_eq(b"abc", b"abd"));
        assert!(!secret_eq(b"abc", b"ab"));
        assert!(!secret_eq(b"", b"a"));
        assert!(secret_eq(b"", b""));
    }

    #[test]
    fn a_secret_is_hex_of_the_length_asked_for() {
        let secret = random_hex(32).expect("a secret");
        assert_eq!(secret.len(), 64, "32 bytes is 64 hex digits");
        assert!(secret.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(secret, random_hex(32).expect("a second secret"));
    }

    #[test]
    fn a_private_file_is_written_without_a_reader_seeing_it_half_done() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("nested").join("state.json");
        write_private_atomic(&path, b"{\"a\":1}").expect("the file is written");
        assert_eq!(
            std::fs::read_to_string(&path).expect("the file"),
            "{\"a\":1}"
        );
        assert!(
            !path.with_extension("tmp").exists(),
            "the temporary file is renamed, not left behind"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .expect("the file")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the file is private");
            let dir_mode = std::fs::metadata(path.parent().expect("a parent"))
                .expect("the dir")
                .permissions()
                .mode();
            assert_eq!(dir_mode & 0o777, 0o700, "the directory is private");
        }
    }
}
