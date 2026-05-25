use crate::qdrant_store::{QdrantStore, collection_name};
use crate::store::{LanceDbStore, SearchFilter, SemanticMatch, StoredChunk, table_name};
use anyhow::{Result, bail};
use std::path::Path;

const BACKEND_ENV: &str = "MCP_SEMANTIC_BACKEND";
const QDRANT_MANIFEST_FILE: &str = "manifest.qdrant.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SemanticBackend {
    LanceDb,
    Qdrant,
}

pub(crate) enum SemanticStore {
    LanceDb(LanceDbStore),
    Qdrant(QdrantStore),
}

impl SemanticBackend {
    pub(crate) fn from_env() -> Result<Self> {
        match std::env::var(BACKEND_ENV) {
            Ok(value) if value.trim().eq_ignore_ascii_case("qdrant") => Ok(Self::Qdrant),
            Ok(value)
                if value.trim().is_empty() || value.trim().eq_ignore_ascii_case("lancedb") =>
            {
                Ok(Self::LanceDb)
            }
            Ok(value) => bail!(
                "unsupported semantic backend {value:?}; expected unset, 'lancedb', or 'qdrant'"
            ),
            Err(std::env::VarError::NotPresent) => Ok(Self::LanceDb),
            Err(err) => Err(err.into()),
        }
    }

    pub(crate) fn manifest_file(self) -> &'static str {
        match self {
            Self::LanceDb => "manifest.json",
            Self::Qdrant => QDRANT_MANIFEST_FILE,
        }
    }

    pub(crate) fn index_name(self, model_slug: &str, vector_dim: usize) -> String {
        match self {
            Self::LanceDb => table_name(model_slug, vector_dim),
            Self::Qdrant => collection_name(model_slug, vector_dim),
        }
    }

    pub(crate) fn store_location(self, index_dir: &Path, index_name: &str) -> String {
        match self {
            Self::LanceDb => index_dir.display().to_string(),
            Self::Qdrant => format!("qdrant:{index_name}"),
        }
    }
}

impl SemanticStore {
    pub(crate) async fn open_or_create(
        backend: SemanticBackend,
        index_dir: &Path,
        model_slug: &str,
        vector_dim: usize,
    ) -> Result<Self> {
        match backend {
            SemanticBackend::LanceDb => {
                LanceDbStore::open_or_create(index_dir, model_slug, vector_dim)
                    .await
                    .map(Self::LanceDb)
            }
            SemanticBackend::Qdrant => {
                QdrantStore::open_or_create(collection_name(model_slug, vector_dim), vector_dim)
                    .await
                    .map(Self::Qdrant)
            }
        }
    }

    pub(crate) async fn open_existing(
        backend: SemanticBackend,
        index_dir: &Path,
        index_name: &str,
        vector_dim: usize,
    ) -> Result<Self> {
        match backend {
            SemanticBackend::LanceDb => {
                LanceDbStore::open_existing(index_dir, index_name, vector_dim)
                    .await
                    .map(Self::LanceDb)
            }
            SemanticBackend::Qdrant => QdrantStore::open_existing(index_name.to_string())
                .await
                .map(Self::Qdrant),
        }
    }

    pub(crate) async fn delete_paths(&self, root: &str, paths: &[String]) -> Result<usize> {
        match self {
            Self::LanceDb(store) => store.delete_paths(root, paths).await,
            Self::Qdrant(store) => store.delete_paths(root, paths).await,
        }
    }

    pub(crate) async fn add_chunks(&self, records: Vec<StoredChunk>) -> Result<()> {
        match self {
            Self::LanceDb(store) => store.add_chunks(records).await,
            Self::Qdrant(store) => store.add_chunks(records).await,
        }
    }

    pub(crate) async fn search(
        &self,
        query_embedding: Vec<f32>,
        filter: SearchFilter,
    ) -> Result<Vec<SemanticMatch>> {
        match self {
            Self::LanceDb(store) => store.search(query_embedding, filter).await,
            Self::Qdrant(store) => store.search(query_embedding, filter).await,
        }
    }
}
