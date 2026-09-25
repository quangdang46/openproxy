//! Durable storage for revoked dashboard JWT ids and the bulk-invalidation
//! token epoch.
//!
//! 9router keeps exactly one piece of auth state across a restart: the signing
//! secret, written under `DATA_DIR` and re-read on every boot
//! (`src/lib/auth/dashboardSession.js:12-22`). It never revokes server-side —
//! logout only deletes the cookie (`dashboardSession.js:72-74`) — so it has no
//! revocation list to persist and no epoch counter at all. OpenProxy is
//! strictly stronger on both counts, and the same DATA_DIR contract is what
//! stops that extra state from evaporating on the next boot.
//!
//! Two files under the data directory, laid out like
//! [`crate::server::auth::login_limiter`]:
//!
//! - `revoked-jtis.jsonl` — one `{"jti":…,"revoked_at":…}` per line. Appended on
//!   logout so a revocation is O(1) and never blocks on the size of the set,
//!   then rewritten in full by [`RevocationStore::prune`] to compact.
//! - `token-epoch` — the bare integer epoch, replaced atomically on bump.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};

/// A revocation record older than this can never match a live token: every
/// issuer mints a 24h dashboard JWT, so anything past the documented 7-day
/// token ceiling is dead weight on disk and in memory alike.
pub const JTI_CLEANUP_TTL_SECS: u64 = 7 * 24 * 60 * 60;

/// Unix seconds since the epoch, saturating at 0 if the clock is before it.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// On-disk line shape of `revoked-jtis.jsonl`. Append and prune both serialise
/// this type, so the file round-trips through [`RevocationStore::load`]
/// unchanged no matter which path wrote it.
#[derive(Debug, Serialize, Deserialize)]
struct StoredRevocation {
    jti: String,
    revoked_at: u64,
}

/// Revoked `jti` values plus the current token epoch, rooted at a data
/// directory. Everything the process needs to answer "is this dashboard session
/// still valid" after a restart.
pub struct RevocationStore {
    jtis_path: PathBuf,
    epoch_path: PathBuf,
    jtis: DashMap<String, u64>,
    epoch: AtomicU64,
    loaded: AtomicBool,
}

impl RevocationStore {
    /// Create a store whose files live under `data_dir`. Nothing touches the
    /// disk until [`load`](Self::load) or a mutating call runs; creating the
    /// directory itself is the caller's job, exactly as `Db::load_from` does
    /// for the limiter beside it.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        Self {
            jtis_path: data_dir.join("revoked-jtis.jsonl"),
            epoch_path: data_dir.join("token-epoch"),
            jtis: DashMap::new(),
            epoch: AtomicU64::new(0),
            loaded: AtomicBool::new(false),
        }
    }

    /// Read the persisted state off disk. Idempotent, and a missing or corrupt
    /// file degrades to "nothing revoked yet" rather than an error — a torn
    /// revocation file must never keep the dashboard locked out.
    pub fn load(&self) {
        if self.loaded.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Ok(contents) = std::fs::read_to_string(&self.jtis_path) {
            for line in contents.lines().filter(|l| !l.trim().is_empty()) {
                match serde_json::from_str::<StoredRevocation>(line) {
                    Ok(record) => {
                        self.jtis.insert(record.jti, record.revoked_at);
                    }
                    Err(e) => {
                        tracing::warn!(?e, "RevocationStore: skipping malformed record");
                    }
                }
            }
        }
        // An absent or unparsable epoch file means a fresh install (epoch 0),
        // not a broken one.
        if let Ok(raw) = std::fs::read_to_string(&self.epoch_path) {
            if let Ok(epoch) = raw.trim().parse::<u64>() {
                self.epoch.store(epoch, Ordering::Relaxed);
            }
        }
    }

    /// Revoke a dashboard session by its `jti`. Idempotent: a jti is recorded
    /// once, so a repeated logout neither rewrites the timestamp nor appends a
    /// duplicate line.
    ///
    /// The append happens before this returns so a crash cannot leave a logged
    /// -out cookie accepted on the next boot.
    pub fn revoke(&self, jti: &str) {
        let now = now_unix();
        match self.jtis.entry(jti.to_string()) {
            Entry::Occupied(_) => return,
            Entry::Vacant(slot) => {
                slot.insert(now);
            }
        }
        match serde_json::to_string(&StoredRevocation {
            jti: jti.to_string(),
            revoked_at: now,
        }) {
            Ok(mut line) => {
                line.push('\n');
                self.append(&line);
            }
            Err(e) => tracing::warn!(?e, "RevocationStore: failed to serialize revocation"),
        }
    }

    /// True when this session was individually revoked (per-session logout).
    pub fn is_revoked(&self, jti: &str) -> bool {
        self.jtis.contains_key(jti)
    }

    /// The current bulk-invalidation epoch.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Relaxed)
    }

    /// Mint a `jti` under the current epoch, as `<epoch>:<uuid>`.
    pub fn generate_jti(&self) -> String {
        let id = uuid::Uuid::new_v4();
        format!("{}:{}", self.epoch(), id)
    }

    /// Parse a `jti` and check whether its epoch still matches. A jti with no
    /// epoch segment, or one that is not a number, is simply invalid — never a
    /// panic, because this runs on every authenticated dashboard request.
    pub fn is_jti_valid(&self, jti: &str) -> bool {
        let Some(epoch_str) = jti.split(':').next() else {
            return false;
        };
        let Ok(epoch) = epoch_str.parse::<u64>() else {
            return false;
        };
        epoch == self.epoch()
    }

    /// Invalidate every token issued so far by advancing the epoch, and make
    /// the new value durable before returning — the promise of "all sessions
    /// have been invalidated" has to survive a restart, not just outlive the
    /// response that made it.
    ///
    /// Returns the new epoch. A write failure is logged and swallowed so auth
    /// is never blocked by the disk; the in-memory bump still takes effect.
    pub fn bump_epoch(&self) -> u64 {
        let next = self.epoch.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        self.set_epoch(next);
        next
    }

    /// Force the epoch to `epoch` and make it durable. Only used to adopt a
    /// bump that happened before this store existed; [`bump_epoch`](Self::bump_epoch)
    /// is the normal path.
    pub fn set_epoch(&self, epoch: u64) {
        self.epoch.store(epoch, Ordering::Relaxed);
        write_atomic(
            &self.epoch_path,
            epoch.to_string().as_bytes(),
            ".token-epoch",
        );
    }

    /// Drop revocations older than [`JTI_CLEANUP_TTL_SECS`] and compact the
    /// file. Without the rewrite an append-only log grows for the life of the
    /// install and every boot re-reads dead entries.
    ///
    /// The file is only touched when its contents would actually change, so the
    /// hourly tick does no I/O on a quiet install — and an unchanged set is
    /// guaranteed byte-identical rather than incidentally so.
    ///
    /// Returns the number of entries removed.
    pub fn prune(&self, now: u64) -> usize {
        let cutoff = now.saturating_sub(JTI_CLEANUP_TTL_SECS);
        let before = self.jtis.len();
        self.jtis.retain(|_jti, revoked_at| *revoked_at > cutoff);
        let removed = before - self.jtis.len();

        let mut survivors: Vec<StoredRevocation> = self
            .jtis
            .iter()
            .map(|entry| StoredRevocation {
                jti: entry.key().clone(),
                revoked_at: *entry.value(),
            })
            .collect();
        // Sorted so the output does not depend on DashMap's iteration order;
        // without that, the same set would serialise differently on every tick.
        survivors.sort_by(|a, b| a.jti.cmp(&b.jti));

        let mut buf = String::new();
        for record in &survivors {
            match serde_json::to_string(record) {
                Ok(line) => {
                    buf.push_str(&line);
                    buf.push('\n');
                }
                Err(e) => tracing::warn!(?e, "RevocationStore: failed to serialize revocation"),
            }
        }
        // Comparing first also compacts records the load skipped as malformed:
        // they never entered `jtis`, so `removed` is 0 but the file still needs
        // rewriting to shed them.
        let unchanged = matches!(
            std::fs::read_to_string(&self.jtis_path),
            Ok(existing) if existing == buf
        );
        if !unchanged {
            write_atomic(&self.jtis_path, buf.as_bytes(), ".revoked-jtis");
        }
        removed
    }

    fn append(&self, line: &str) {
        let mut file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.jtis_path)
        {
            Ok(file) => file,
            Err(e) => {
                tracing::warn!(?e, "RevocationStore: failed to open revocations file");
                return;
            }
        };
        if let Err(e) = file.write_all(line.as_bytes()) {
            tracing::warn!(?e, "RevocationStore: failed to append revocation");
        }
    }
}

/// Replace `path` with `bytes` via write-then-rename, so a reader can never
/// observe a half-written file. Failures are logged and swallowed for the same
/// reason `LoginLimiter::persist` does: a write error must not take auth down.
fn write_atomic(path: &Path, bytes: &[u8], tmp_label: &str) {
    let root = PathBuf::new();
    let dir = path.parent().unwrap_or(&root);
    let tmp = dir.join(format!("{}.{}.tmp", tmp_label, uuid::Uuid::new_v4()));

    if let Err(e) = std::fs::write(&tmp, bytes) {
        tracing::warn!(?e, path = %path.display(), "RevocationStore: failed to write file");
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        tracing::warn!(?e, path = %path.display(), "RevocationStore: failed to rename file");
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::{now_unix, RevocationStore};
    use std::io::Write;
    use tempfile::tempdir;

    /// A logout that the next boot forgets is a logged-out cookie that works
    /// again. The second store over the same directory is the restart.
    #[test]
    fn revoked_jti_is_still_revoked_after_a_restart() {
        let dir = tempdir().unwrap();

        let before = RevocationStore::new(dir.path());
        before.load();
        before.revoke("3:aaaaaaaa-bbbb");
        assert!(before.jtis_path.exists());

        let after = RevocationStore::new(dir.path());
        after.load();
        assert!(
            after.is_revoked("3:aaaaaaaa-bbbb"),
            "a logged-out session must stay logged out across a restart"
        );
        assert!(!after.is_revoked("3:some-other-session"));
    }

    /// `increment_token_epoch` backs the "all sessions have been invalidated"
    /// promise on password change. If the counter resets, every session the
    /// user was told was killed comes back to life on the next boot.
    #[test]
    fn token_epoch_survives_a_restart_and_rejects_pre_change_tokens() {
        let dir = tempdir().unwrap();
        let old_jti = "0:11111111-1111-1111-1111-111111111111";

        let process_a = RevocationStore::new(dir.path());
        process_a.load();
        assert!(process_a.is_jti_valid(old_jti));

        process_a.bump_epoch();
        assert!(!process_a.is_jti_valid(old_jti));
        let fresh = process_a.generate_jti();

        let process_b = RevocationStore::new(dir.path());
        process_b.load();
        assert_eq!(process_b.epoch(), 1);
        assert!(
            !process_b.is_jti_valid(old_jti),
            "a password change must still invalidate pre-change tokens after a restart"
        );
        assert!(
            process_b.is_jti_valid(&fresh),
            "tokens minted after the change must keep working after a restart"
        );
    }

    /// A jti whose epoch segment is not a number is rejected, not fatal.
    #[test]
    fn malformed_jti_is_invalid_not_fatal() {
        let dir = tempdir().unwrap();
        let store = RevocationStore::new(dir.path());
        store.load();
        assert!(!store.is_jti_valid("not-an-epoch:uuid"));
        assert!(!store.is_jti_valid(""));
    }

    /// Without the write-back the append-only file only ever grows, and every
    /// boot replays a stale revocation that can no longer match a live token.
    #[test]
    fn prune_rewrites_the_file_so_stale_entries_do_not_come_back() {
        let dir = tempdir().unwrap();
        let first_run = RevocationStore::new(dir.path());
        first_run.load();
        first_run.revoke("9:live-jti");

        // A record from an earlier run, written straight into the file so the
        // test does not have to wait seven days for one to age out.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join("revoked-jtis.jsonl"))
            .unwrap();
        file.write_all(b"{\"jti\":\"9:ancient-jti\",\"revoked_at\":1}\n")
            .unwrap();
        drop(file);

        // The next boot, so the stale record is in memory the way it is on the
        // real hourly tick after a restart.
        let store = RevocationStore::new(dir.path());
        store.load();
        let removed = store.prune(now_unix());
        assert_eq!(removed, 1);

        let contents = std::fs::read_to_string(dir.path().join("revoked-jtis.jsonl")).unwrap();
        assert!(!contents.contains("9:ancient-jti"));
        assert!(contents.contains("9:live-jti"));

        let restarted = RevocationStore::new(dir.path());
        restarted.load();
        assert!(!restarted.is_revoked("9:ancient-jti"));
        assert!(restarted.is_revoked("9:live-jti"));
    }

    /// The cutoff is exclusive (`inserted_at > cutoff`). An off-by-one here
    /// would mass-revoke live sessions every hour, so pin the quiet case: a set
    /// with nothing stale comes back byte-identical.
    #[test]
    fn prune_keeps_everything_when_nothing_is_stale() {
        let dir = tempdir().unwrap();
        let store = RevocationStore::new(dir.path());
        store.load();
        store.revoke("4:fresh-jti");

        let path = dir.path().join("revoked-jtis.jsonl");
        let before = std::fs::read(&path).unwrap();
        assert_eq!(store.prune(now_unix()), 0);
        assert_eq!(std::fs::read(&path).unwrap(), before);

        let restarted = RevocationStore::new(dir.path());
        restarted.load();
        assert!(restarted.is_revoked("4:fresh-jti"));
    }

    /// A line the loader skipped never entered the map, so the retain cannot
    /// count it as removed — only the rewrite sheds it. Pin that, or corrupt
    /// lines accumulate for the life of the install.
    #[test]
    fn prune_sheds_records_the_loader_could_not_read() {
        let dir = tempdir().unwrap();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.path().join("revoked-jtis.jsonl"))
            .unwrap();
        file.write_all(b"not json at all\n").unwrap();
        drop(file);

        let store = RevocationStore::new(dir.path());
        store.load();
        assert_eq!(
            store.prune(now_unix()),
            0,
            "a corrupt line is not a revoked jti"
        );

        let contents = std::fs::read_to_string(dir.path().join("revoked-jtis.jsonl")).unwrap();
        assert!(
            contents.is_empty(),
            "the corrupt line should have been compacted away, got {contents:?}"
        );
    }
}
