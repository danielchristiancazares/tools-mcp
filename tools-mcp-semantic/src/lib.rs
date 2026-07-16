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
    if std::env::var_os(backend::BACKEND_ENV).is_none() {
        return;
    }

    tools::register_tools(registry);
}

/// Bench-only surface. Wraps internal embedding APIs so the benchmark harness can measure
/// embedding throughput directly without routing through the full `SemanticIndex` pipeline. Not
/// for production use; the surface here is unstable and may change without notice.
#[cfg(feature = "bench-api")]
pub mod bench {
    use anyhow::Result;
    use std::path::Path;
    use std::path::PathBuf;
    use tools_mcp_core::ToolRegistry;

    use crate::chunking::{chunk_source, hash_bytes};
    use crate::discovery::FileCandidate;
    use crate::embedding::FastEmbedProvider as InternalProvider;

    /// Bench-only tool registration bypasses the runtime `MCP_SEMANTIC_BACKEND` startup gate so
    /// benchmark invocations keep measuring the semantic tools directly.
    pub fn register_tools(registry: &mut ToolRegistry) {
        crate::tools::register_tools(registry);
    }

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

    /// Bench-only wrapper around Markdown chunking so we can isolate the hot path from discovery
    /// and embedding overhead.
    pub fn chunk_markdown(markdown: &str) -> usize {
        let file = FileCandidate {
            absolute_path: PathBuf::from("bench.md"),
            relative_path: "bench.md".to_string(),
            language: "markdown".to_string(),
            size: markdown.len() as u64,
            modified: None,
        };
        let file_hash = hash_bytes(markdown.as_bytes());
        chunk_source(&file, markdown, &file_hash).len()
    }

    pub fn delete_paths_predicate_len(root: &str, paths: &[String]) -> usize {
        crate::store::benchmark_delete_paths_predicate_len(root, paths)
    }

    pub fn directory_filter_predicate_len(
        root: &str,
        directory: &str,
        language: Option<&str>,
    ) -> usize {
        crate::store::benchmark_directory_filter_predicate_len(root, directory, language)
    }
}
