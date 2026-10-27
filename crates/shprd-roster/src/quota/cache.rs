//! On-disk quota cache: `<home>/.shprd/cache/quota.json`.
//!
//! Ported from og installer/og_quota.py.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::quota::QuotaReport;

/// How long a stored report is served without probing again.
pub const CACHE_TTL_MS: u64 = 120_000;

/// How old a last-good report may be and still stand in for a failed probe.
pub const STALE_TTL_MS: u64 = 15 * 60 * 1000;

/// Wall clock in epoch milliseconds.
#[must_use]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The quota cache file, keyed by agent id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cache {
    path: PathBuf,
}

impl Cache {
    /// Cache inside `home` (`<home>/.shprd/cache/quota.json`).
    #[must_use]
    pub fn new(home: &Path) -> Self {
        Self {
            path: home.join(".shprd").join("cache").join("quota.json"),
        }
    }

    /// Path of the cache file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every stored record. An unreadable or corrupt file reads as empty: a
    /// broken cache must never fail a probe.
    #[must_use]
    pub fn load(&self) -> BTreeMap<String, QuotaReport> {
        fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// The stored record for `agent` if it is younger than [`CACHE_TTL_MS`].
    #[must_use]
    pub fn fresh(&self, agent: &str, now_ms: u64) -> Option<QuotaReport> {
        self.load()
            .remove(&key(agent))
            .filter(|r| r.age_ms(now_ms) < CACHE_TTL_MS)
    }

    /// The stored record for `agent` if it is younger than [`STALE_TTL_MS`];
    /// this is the fallback after a 429 or a network failure.
    #[must_use]
    pub fn last_good(&self, agent: &str, now_ms: u64) -> Option<QuotaReport> {
        self.load()
            .remove(&key(agent))
            .filter(|r| r.age_ms(now_ms) < STALE_TTL_MS)
    }

    /// Store `report`, atomically: a unique temp file in the same directory is
    /// written and renamed over the cache.
    pub fn store(&self, report: &QuotaReport) -> io::Result<()> {
        let mut all = self.load();
        all.insert(key(&report.agent), report.clone());
        let body = serde_json::to_string_pretty(&all).map_err(io::Error::other)?;
        let dir = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("cache path has no parent"))?;
        fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(
            "quota.json.{}.{}.tmp",
            std::process::id(),
            now_ms()
        ));
        fs::write(&tmp, body)?;
        fs::rename(&tmp, &self.path).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
    }
}

/// Cache key for an agent id (ids are matched case-insensitively).
fn key(agent: &str) -> String {
    agent.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::{QuotaState, Tier, Window};

    fn report(agent: &str, checked_at_ms: u64) -> QuotaReport {
        QuotaReport {
            agent: agent.into(),
            state: QuotaState::Ok,
            tier: Tier::Measured,
            windows: vec![Window {
                name: "five_hour".into(),
                used_percent: 40.0,
                reset_at_ms: Some(checked_at_ms + 60_000),
            }],
            reset_at_ms: Some(checked_at_ms + 60_000),
            source: "anthropic-oauth".into(),
            detail: "1 window(s); five_hour binding".into(),
            checked_at_ms,
        }
    }

    fn temp_dir() -> tempfile::TempDir {
        match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("should create a temp dir: {error}"),
        }
    }

    fn store(cache: &Cache, report: &QuotaReport) {
        if let Err(error) = cache.store(report) {
            panic!("should store report: {error}");
        }
    }

    fn dir_of(cache: &Cache) -> PathBuf {
        match cache.path().parent() {
            Some(dir) => dir.to_path_buf(),
            None => panic!("cache path should have a parent"),
        }
    }

    #[test]
    fn missing_cache_reads_as_empty() {
        let dir = temp_dir();
        let cache = Cache::new(dir.path());
        assert!(cache.load().is_empty());
        assert_eq!(cache.fresh("claude", 0), None);
        assert!(cache.path().ends_with(".shprd/cache/quota.json"));
    }

    #[test]
    fn store_roundtrips_and_leaves_no_temp_file() {
        let dir = temp_dir();
        let cache = Cache::new(dir.path());
        store(&cache, &report("claude", 1_000));
        assert_eq!(cache.load().get("claude"), Some(&report("claude", 1_000)));
        let left: Vec<_> = match std::fs::read_dir(dir_of(&cache)) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".tmp"))
                .collect(),
            Err(error) => panic!("should read cache dir: {error}"),
        };
        assert!(left.is_empty(), "temp files left: {left:?}");
        // Keyed by agent, so a second agent does not clobber the first.
        store(&cache, &report("codex", 2_000));
        assert_eq!(cache.load().len(), 2);
    }

    #[test]
    fn fresh_and_last_good_honour_their_ttls() {
        let dir = temp_dir();
        let cache = Cache::new(dir.path());
        store(&cache, &report("claude", 10_000));
        assert!(cache.fresh("claude", 10_000 + CACHE_TTL_MS - 1).is_some());
        assert!(cache.fresh("claude", 10_000 + CACHE_TTL_MS).is_none());
        assert!(
            cache
                .last_good("claude", 10_000 + STALE_TTL_MS - 1)
                .is_some()
        );
        assert!(cache.last_good("claude", 10_000 + STALE_TTL_MS).is_none());
        assert!(cache.fresh("codex", 10_000).is_none());
        assert!(cache.last_good("claude", 0).is_some());
    }

    #[test]
    fn corrupt_cache_is_ignored() {
        let dir = temp_dir();
        let cache = Cache::new(dir.path());
        if let Err(error) = std::fs::create_dir_all(dir_of(&cache)) {
            panic!("should create cache dir: {error}");
        }
        if let Err(error) = std::fs::write(cache.path(), "{ not json") {
            panic!("should write corrupt cache: {error}");
        }
        assert!(cache.load().is_empty());
        store(&cache, &report("claude", 5));
        assert_eq!(cache.load().len(), 1);
    }
}
