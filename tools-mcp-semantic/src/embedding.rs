use anyhow::{Context, Result, anyhow};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

const DEFAULT_MODEL_ID: &str = "jina-embeddings-v2-base-code";
const DEFAULT_MODEL_SLUG: &str = "jina_embeddings_v2_base_code";
const CPU_EMBEDDING_BATCH_SIZE: usize = 32;
const CUDA_EMBEDDING_BATCH_SIZE: usize = 128;

type SharedModel = Arc<Mutex<TextEmbedding>>;

static MODEL_CACHE: OnceLock<Mutex<HashMap<PathBuf, SharedModel>>> = OnceLock::new();

#[derive(Clone)]
pub(crate) struct FastEmbedProvider {
    model_id: String,
    model_slug: String,
    model: SharedModel,
}

impl FastEmbedProvider {
    pub(crate) async fn new(index_dir: &Path) -> Result<Self> {
        let model_id = default_model_id().to_string();
        let model_slug = default_model_slug().to_string();
        let cache_dir = index_dir.join("models");

        if let Some(model) = cached_model(&cache_dir)? {
            return Ok(Self {
                model_id,
                model_slug,
                model,
            });
        }

        let initialized = initialize_model(cache_dir.clone(), model_id.clone()).await?;
        let model = {
            let mut cache = model_cache()
                .lock()
                .map_err(|_| anyhow!("FastEmbed model cache lock was poisoned"))?;
            cache
                .entry(cache_dir)
                .or_insert_with(|| initialized)
                .clone()
        };

        Ok(Self {
            model_id,
            model_slug,
            model,
        })
    }

    pub(crate) fn model_id(&self) -> &str {
        &self.model_id
    }

    pub(crate) fn model_slug(&self) -> &str {
        &self.model_slug
    }

    pub(crate) async fn embed_documents(&self, documents: Vec<String>) -> Result<Vec<Vec<f32>>> {
        self.embed_prefixed(
            documents,
            "passage: ",
            "index documents",
            default_embedding_batch_size(),
        )
        .await
    }

    pub(crate) async fn embed_query(&self, query: String) -> Result<Vec<f32>> {
        let mut embeddings = self
            .embed_prefixed(
                vec![query],
                "query: ",
                "search query",
                default_embedding_batch_size(),
            )
            .await
            .context("failed to embed semantic search query")?;
        embeddings
            .pop()
            .ok_or_else(|| anyhow!("FastEmbed returned no query embedding"))
    }

    /// Embed documents with an explicit internal batch size. Used by the bench harness to
    /// measure how FastEmbed/ONNX throughput varies with the internal batch size; production
    /// callers should use `embed_documents`.
    #[cfg(feature = "bench-api")]
    pub(crate) async fn embed_documents_with_batch_size(
        &self,
        documents: Vec<String>,
        batch_size: usize,
    ) -> Result<Vec<Vec<f32>>> {
        self.embed_prefixed(documents, "passage: ", "index documents", batch_size)
            .await
    }

    async fn embed_prefixed(
        &self,
        texts: Vec<String>,
        prefix: &'static str,
        operation: &'static str,
        batch_size: usize,
    ) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let provider = self.clone();
        tokio::task::spawn_blocking(move || {
            let prepared = texts
                .into_iter()
                .map(|text| {
                    let mut prepared = String::with_capacity(prefix.len() + text.len());
                    prepared.push_str(prefix);
                    prepared.push_str(&text);
                    prepared
                })
                .collect::<Vec<_>>();
            let mut model = provider
                .model
                .lock()
                .map_err(|_| anyhow!("FastEmbed model lock was poisoned"))?;
            model
                .embed(prepared, Some(batch_size))
                .with_context(|| format!("failed to embed semantic {operation}"))
        })
        .await
        .context("semantic embedding task failed")?
    }
}

pub(crate) fn default_model_id() -> &'static str {
    DEFAULT_MODEL_ID
}

pub(crate) fn default_model_slug() -> &'static str {
    DEFAULT_MODEL_SLUG
}

pub(crate) fn default_embedding_batch_size() -> usize {
    if cfg!(feature = "gpu-cuda") {
        CUDA_EMBEDDING_BATCH_SIZE
    } else {
        CPU_EMBEDDING_BATCH_SIZE
    }
}

async fn initialize_model(cache_dir: PathBuf, model_id: String) -> Result<SharedModel> {
    tokio::task::spawn_blocking(move || {
        // `mut` is only needed in the gpu-cuda configuration; suppress the warning on CPU builds.
        #[allow(unused_mut)]
        let mut options = InitOptions::new(EmbeddingModel::JinaEmbeddingsV2BaseCode)
            .with_cache_dir(cache_dir)
            .with_show_download_progress(false);

        #[cfg(feature = "gpu-cuda")]
        {
            // Registers NVIDIA CUDA as the preferred execution provider. ORT falls back to CPU
            // silently if registration fails (e.g. missing CUDA libraries), which would silently
            // erase the GPU signal in benchmarks. `.error_on_failure()` makes that case loud.
            use ort::ep::CUDA;
            options =
                options.with_execution_providers(vec![CUDA::default().build().error_on_failure()]);
        }

        let model = TextEmbedding::try_new(options)
            .with_context(|| format!("failed to initialize FastEmbed model {model_id}"))?;
        Ok(Arc::new(Mutex::new(model)))
    })
    .await
    .context("FastEmbed initialization task failed")?
}

fn cached_model(cache_dir: &Path) -> Result<Option<SharedModel>> {
    let cache = model_cache()
        .lock()
        .map_err(|_| anyhow!("FastEmbed model cache lock was poisoned"))?;
    Ok(cache.get(cache_dir).cloned())
}

fn model_cache() -> &'static Mutex<HashMap<PathBuf, SharedModel>> {
    MODEL_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(test)]
fn slugify_model_id(model_id: &str) -> String {
    model_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_MODEL_ID, default_embedding_batch_size, default_model_slug, slugify_model_id,
    };

    #[test]
    fn model_slug_is_table_safe() {
        assert_eq!(
            slugify_model_id("jinaai/jina-embeddings-v2-base-code"),
            "jinaai_jina_embeddings_v2_base_code"
        );
    }

    #[test]
    fn default_model_slug_is_stable() {
        assert_eq!(slugify_model_id(DEFAULT_MODEL_ID), default_model_slug());
    }

    #[test]
    fn default_embedding_batch_size_matches_execution_provider() {
        let expected = if cfg!(feature = "gpu-cuda") { 128 } else { 32 };
        assert_eq!(default_embedding_batch_size(), expected);
    }
}
