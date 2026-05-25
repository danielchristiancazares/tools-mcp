mod backend;
mod chunking;
mod discovery;
mod embedding;
mod manifest;
mod model;
mod qdrant_store;
mod store;
mod tools;

use tools_mcp_core::ToolRegistry;

pub fn register_tools(registry: &mut ToolRegistry) {
    tools::register_tools(registry);
}

/// Bench-only surface. Wraps internal embedding APIs so the benchmark harness can measure
/// embedding throughput directly without routing through the full `SemanticIndex` pipeline. Not
/// for production use; the surface here is unstable and may change without notice.
#[cfg(feature = "bench-api")]
pub mod bench {
    use anyhow::Result;
    use std::path::Path;

    use crate::embedding::FastEmbedProvider as InternalProvider;

    /// Public-for-bench wrapper around the internal FastEmbed provider. Mirrors only the methods
    /// the bench harness needs; intentionally not a full re-export.
    pub struct FastEmbedProvider(InternalProvider);

    impl FastEmbedProvider {
        pub async fn new(index_dir: &Path) -> Result<Self> {
            InternalProvider::new(index_dir).await.map(Self)
        }

        pub async fn embed_documents(&self, documents: Vec<String>) -> Result<Vec<Vec<f32>>> {
            self.0.embed_documents(documents).await
        }

        pub async fn embed_documents_with_batch_size(
            &self,
            documents: Vec<String>,
            batch_size: usize,
        ) -> Result<Vec<Vec<f32>>> {
            self.0
                .embed_documents_with_batch_size(documents, batch_size)
                .await
        }

        pub fn model_id(&self) -> &str {
            self.0.model_id()
        }
    }
}
