use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, ErrorKind, Write};
use std::path::{Path, PathBuf};

const MANIFEST_VERSION: u32 = 1;
const DEFAULT_MANIFEST_FILE: &str = "manifest.json";

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct IndexManifest {
    pub(crate) version: u32,
    pub(crate) workspace: String,
    pub(crate) model_id: String,
    pub(crate) table_name: Option<String>,
    pub(crate) vector_dim: Option<usize>,
    pub(crate) files: BTreeMap<String, ManifestFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ManifestFile {
    pub(crate) file_hash: String,
    pub(crate) chunk_ids: Vec<String>,
    pub(crate) indexed_at: String,
    /// File size when the content hash was recorded. Optional and additive: manifests written
    /// by older builds lack it and fall back to the content-hash check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) size: Option<u64>,
    /// File mtime (nanoseconds since the Unix epoch) when the content hash was recorded.
    /// Only stored when the mtime was old enough at record time to be race-free (see
    /// `model::recordable_stamp`); `None` forces the content-hash check on the next run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mtime_unix_nanos: Option<u64>,
}

impl IndexManifest {
    pub(crate) fn load_or_new(
        index_dir: &Path,
        model_slug: &str,
        workspace: &Path,
        model_id: &str,
    ) -> Result<Self> {
        Self::load_or_new_named(
            index_dir,
            model_slug,
            DEFAULT_MANIFEST_FILE,
            workspace,
            model_id,
        )
    }

    pub(crate) fn load_or_new_named(
        index_dir: &Path,
        model_slug: &str,
        manifest_file: &str,
        workspace: &Path,
        model_id: &str,
    ) -> Result<Self> {
        let path = manifest_path_named(index_dir, model_slug, manifest_file);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                return Ok(Self::new(workspace, model_id));
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("failed to read semantic index manifest {}", path.display())
                });
            }
        };

        let reader = BufReader::new(file);
        let mut parsed: Self = serde_json::from_reader(reader).with_context(|| {
            format!("failed to parse semantic index manifest {}", path.display())
        })?;
        if parsed.version == 0 {
            parsed.version = MANIFEST_VERSION;
        }
        Ok(parsed)
    }

    pub(crate) fn save(&self, index_dir: &Path, model_slug: &str) -> Result<()> {
        self.save_named(index_dir, model_slug, DEFAULT_MANIFEST_FILE)
    }

    pub(crate) fn save_named(
        &self,
        index_dir: &Path,
        model_slug: &str,
        manifest_file: &str,
    ) -> Result<()> {
        let path = manifest_path_named(index_dir, model_slug, manifest_file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create manifest directory {}", parent.display())
            })?;
        }
        let file = File::create(&path).with_context(|| {
            format!("failed to write semantic index manifest {}", path.display())
        })?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, self).with_context(|| {
            format!(
                "failed to serialize semantic index manifest {}",
                path.display()
            )
        })?;
        writer
            .flush()
            .with_context(|| format!("failed to write semantic index manifest {}", path.display()))
    }

    pub(crate) fn is_current(&self, path: &str, file_hash: &str) -> bool {
        self.files
            .get(path)
            .is_some_and(|file| file.file_hash == file_hash)
    }

    /// Metadata fast-path: true when the entry recorded a race-free stamp and both size and
    /// mtime match the currently observed values, proving the recorded content hash is still
    /// current without re-reading the file.
    pub(crate) fn is_current_by_stamp(&self, path: &str, size: u64, mtime_unix_nanos: u64) -> bool {
        self.files.get(path).is_some_and(|file| {
            file.size == Some(size) && file.mtime_unix_nanos == Some(mtime_unix_nanos)
        })
    }

    /// Update the stored stamp for a hash-verified entry. Returns true when the stored stamp
    /// actually changed, so callers know the manifest needs persisting.
    pub(crate) fn refresh_stamp(
        &mut self,
        path: &str,
        size: Option<u64>,
        mtime_unix_nanos: Option<u64>,
    ) -> bool {
        let Some(file) = self.files.get_mut(path) else {
            return false;
        };
        if file.size == size && file.mtime_unix_nanos == mtime_unix_nanos {
            return false;
        }
        file.size = size;
        file.mtime_unix_nanos = mtime_unix_nanos;
        true
    }

    pub(crate) fn stale_paths_under(
        &self,
        filter: &crate::discovery::PathFilter,
        discovered: &HashSet<&str>,
    ) -> Vec<String> {
        self.files
            .keys()
            .filter(|path| filter.contains(path.as_str()))
            .filter(|path| !discovered.contains(path.as_str()))
            .cloned()
            .collect()
    }

    pub(crate) fn chunk_count_for_paths<'a>(
        &self,
        paths: impl IntoIterator<Item = &'a String>,
    ) -> usize {
        paths
            .into_iter()
            .filter_map(|path| self.files.get(path))
            .map(|file| file.chunk_ids.len())
            .sum()
    }

    pub(crate) fn remove_paths<I>(&mut self, paths: I)
    where
        I: IntoIterator<Item = String>,
    {
        for path in paths {
            self.files.remove(&path);
        }
    }

    fn new(workspace: &Path, model_id: &str) -> Self {
        Self {
            version: MANIFEST_VERSION,
            workspace: workspace.display().to_string(),
            model_id: model_id.to_string(),
            table_name: None,
            vector_dim: None,
            files: BTreeMap::new(),
        }
    }
}

pub(crate) fn manifest_path_named(
    index_dir: &Path,
    model_slug: &str,
    manifest_file: &str,
) -> PathBuf {
    index_dir.join(model_slug).join(manifest_file)
}

#[cfg(test)]
mod tests {
    use super::{IndexManifest, ManifestFile};
    use std::collections::BTreeMap;

    #[test]
    fn manifest_json_preserves_persisted_schema() {
        let manifest = IndexManifest {
            version: 1,
            workspace: "C:/repo".to_string(),
            model_id: "jinaai/jina-embeddings-v2-base-code".to_string(),
            table_name: Some("semantic_chunks_v1_jina_code_768".to_string()),
            vector_dim: Some(768),
            files: BTreeMap::from([(
                "src/lib.rs".to_string(),
                ManifestFile {
                    file_hash: "file-hash".to_string(),
                    chunk_ids: vec!["chunk-a".to_string(), "chunk-b".to_string()],
                    indexed_at: "2026-05-22T00:00:00Z".to_string(),
                    size: Some(1024),
                    mtime_unix_nanos: Some(1_768_000_000_000_000_000),
                },
            )]),
        };

        let json = serde_json::to_string_pretty(&manifest).expect("serialize manifest");
        assert!(json.contains("\"version\": 1"));
        assert!(json.contains("\"workspace\": \"C:/repo\""));
        assert!(json.contains("\"table_name\": \"semantic_chunks_v1_jina_code_768\""));
        assert!(json.contains("\"vector_dim\": 768"));
        assert!(json.contains("\"chunk_ids\""));
        assert!(json.contains("\"size\": 1024"));
        assert!(json.contains("\"mtime_unix_nanos\": 1768000000000000000"));

        let parsed: IndexManifest = serde_json::from_str(&json).expect("parse manifest");
        assert_eq!(parsed.version, manifest.version);
        assert_eq!(parsed.workspace, manifest.workspace);
        assert_eq!(parsed.model_id, manifest.model_id);
        assert_eq!(parsed.table_name, manifest.table_name);
        assert_eq!(parsed.vector_dim, manifest.vector_dim);
        assert_eq!(
            parsed.files["src/lib.rs"].chunk_ids,
            manifest.files["src/lib.rs"].chunk_ids
        );
        assert_eq!(parsed.files["src/lib.rs"].size, Some(1024));
        assert_eq!(
            parsed.files["src/lib.rs"].mtime_unix_nanos,
            Some(1_768_000_000_000_000_000)
        );
    }

    #[test]
    fn manifest_without_stamps_parses_and_skips_stamp_fast_path() {
        // Manifest written by a pre-stamp build: entries have no size/mtime fields.
        let legacy_json = r#"{
            "version": 1,
            "workspace": "C:/repo",
            "model_id": "jinaai/jina-embeddings-v2-base-code",
            "table_name": "semantic_chunks_v1_jina_code_768",
            "vector_dim": 768,
            "files": {
                "src/lib.rs": {
                    "file_hash": "file-hash",
                    "chunk_ids": ["chunk-a"],
                    "indexed_at": "2026-05-22T00:00:00Z"
                }
            }
        }"#;

        let parsed: IndexManifest = serde_json::from_str(legacy_json).expect("parse legacy");
        assert_eq!(parsed.files["src/lib.rs"].size, None);
        assert_eq!(parsed.files["src/lib.rs"].mtime_unix_nanos, None);
        assert!(!parsed.is_current_by_stamp("src/lib.rs", 1024, 42));
        assert!(parsed.is_current("src/lib.rs", "file-hash"));

        // Serializing a stamp-less entry must not emit null stamp fields.
        let rendered = serde_json::to_string(&parsed).expect("serialize legacy");
        assert!(!rendered.contains("mtime_unix_nanos"));
        assert!(!rendered.contains("\"size\""));
    }

    #[test]
    fn stamp_fast_path_requires_exact_size_and_mtime_match() {
        let mut manifest = IndexManifest {
            version: 1,
            workspace: "C:/repo".to_string(),
            model_id: "model".to_string(),
            table_name: None,
            vector_dim: None,
            files: BTreeMap::from([(
                "a.rs".to_string(),
                ManifestFile {
                    file_hash: "hash".to_string(),
                    chunk_ids: vec![],
                    indexed_at: "2026-07-16T00:00:00Z".to_string(),
                    size: Some(10),
                    mtime_unix_nanos: Some(100),
                },
            )]),
        };

        assert!(manifest.is_current_by_stamp("a.rs", 10, 100));
        assert!(
            !manifest.is_current_by_stamp("a.rs", 11, 100),
            "size differs"
        );
        assert!(
            !manifest.is_current_by_stamp("a.rs", 10, 101),
            "mtime differs"
        );
        assert!(!manifest.is_current_by_stamp("missing.rs", 10, 100));

        assert!(
            !manifest.refresh_stamp("a.rs", Some(10), Some(100)),
            "identical stamp is not a change"
        );
        assert!(manifest.refresh_stamp("a.rs", Some(12), Some(200)));
        assert!(manifest.is_current_by_stamp("a.rs", 12, 200));
        assert!(
            manifest.refresh_stamp("a.rs", Some(12), None),
            "clearing a racy mtime is a change"
        );
        assert!(!manifest.is_current_by_stamp("a.rs", 12, 200));
        assert!(!manifest.refresh_stamp("missing.rs", Some(1), Some(1)));
    }
}
