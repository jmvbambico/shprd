//! Hashing, for the fingerprints the log records as provenance.
//!
//! A hash here names bytes, never a key: it lets a verdict say which findings
//! it decided, so a reader can check that the file the reviewer wrote is the
//! file the gate read.

use sha2::{Digest, Sha256};

/// The SHA-256 of `bytes`, as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut text = String::with_capacity(digest.len() * 2);
    for byte in digest {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    #[test]
    fn a_known_digest_is_lowercase_hex() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn one_changed_byte_changes_the_digest() {
        assert_ne!(sha256_hex(b"[]"), sha256_hex(b"[ ]"));
    }
}
