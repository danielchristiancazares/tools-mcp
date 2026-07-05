//! In-memory record of the last file content the server observed via `Read` (or produced
//! via a successful `Edit`).
//!
//! The store enforces a read-before-edit contract without the caller copying anything
//! between tool calls: `Read` records a file's content hash here, and `Edit` looks it up
//! to confirm (1) the file was read in this server session and (2) it has not changed on
//! disk since. Because a successful `Edit` knows the exact bytes it wrote, it refreshes
//! the snapshot itself, so a chain of edits does not require re-reading.
//!
//! Snapshots are process-scoped: they live for the lifetime of the server process and are
//! keyed by canonical filesystem path so the same file is recognized regardless of how the
//! caller spelled the path.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

static SNAPSHOTS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn store() -> &'static Mutex<HashMap<String, String>> {
    SNAPSHOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Canonical key for a path. Returns `None` when the path cannot be resolved (e.g. the
/// file no longer exists), in which case the snapshot is simply not recorded/found.
fn key(path: &Path) -> Option<String> {
    path.canonicalize()
        .ok()
        .map(|resolved| resolved.to_string_lossy().into_owned())
}

/// SHA-256 of the raw file bytes, formatted `"sha256:<64-hex>"`. Matches the hash format
/// used by `smart_file_edit::model` so snapshot values compare equal to `FileModel` hashes.
pub(crate) fn file_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

/// Record `hash` as the snapshot for `path`.
pub(crate) fn record(path: &Path, hash: String) {
    if let Some(key) = key(path)
        && let Ok(mut snapshots) = store().lock()
    {
        snapshots.insert(key, hash);
    }
}

/// Record the snapshot for `path` from its raw bytes.
pub(crate) fn record_bytes(path: &Path, bytes: &[u8]) {
    record(path, file_hash(bytes));
}

/// Look up the recorded snapshot hash for `path`, if any.
pub(crate) fn get(path: &Path) -> Option<String> {
    let key = key(path)?;
    store().lock().ok()?.get(&key).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn record_then_get_roundtrips_by_canonical_path() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("snap.txt");
        std::fs::write(&path, b"alpha\n").expect("write");

        assert_eq!(get(&path), None);
        record_bytes(&path, b"alpha\n");
        assert_eq!(get(&path), Some(file_hash(b"alpha\n")));
    }

    #[test]
    fn get_is_none_for_unrecorded_file() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("absent.txt");
        std::fs::write(&path, b"x").expect("write");
        assert_eq!(get(&path), None);
    }
}
