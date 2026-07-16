use crate::backend::{SemanticBackend, SemanticStore};
use crate::chunking::{CodeChunk, chunk_source, hash_bytes};
use crate::discovery::{
    DiscoveryOptions, PathFilter, WorkspaceScope, discover_files, discovered_path_set,
    storage_relative_path,
};
use crate::embedding::{
    FastEmbedProvider, default_embedding_batch_size, default_model_id, default_model_slug,
};
use crate::manifest::{IndexManifest, ManifestFile};
use crate::store::{SearchFilter, SemanticMatch, StoredChunk};
use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub(crate) struct IndexOptions {
    pub(crate) path: String,
    pub(crate) force: bool,
    pub(crate) hidden: bool,
    pub(crate) no_ignore: bool,
    pub(crate) limit: usize,
    pub(crate) timeout_ms: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct SearchOptions {
    pub(crate) query: String,
    pub(crate) path: String,
    pub(crate) limit: usize,
    pub(crate) language: Option<String>,
    pub(crate) threshold: Option<f32>,
    pub(crate) include_content: bool,
    pub(crate) timeout_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct IndexSummary {
    pub(crate) indexed_files: usize,
    pub(crate) indexed_chunks: usize,
    pub(crate) updated_files: usize,
    pub(crate) updated_chunks: usize,
    pub(crate) skipped_files: usize,
    pub(crate) deleted_chunks: usize,
    pub(crate) model: String,
    pub(crate) store_path: String,
    pub(crate) duration_ms: u128,
    pub(crate) incremental: bool,
    pub(crate) truncated: bool,
    pub(crate) timed_out: bool,
}

impl IndexSummary {
    pub(crate) fn into_payload(self) -> Value {
        let text = format!(
            "Indexed {} file(s), updated {} file(s); {} chunk(s) indexed, {} chunk(s) written; removed {} stale/replaced chunk(s).",
            self.indexed_files,
            self.updated_files,
            self.indexed_chunks,
            self.updated_chunks,
            self.deleted_chunks
        );
        json!({
            "content": [{"type": "text", "text": text}],
            "isError": false,
            "indexed_files": self.indexed_files,
            "indexed_chunks": self.indexed_chunks,
            "updated_files": self.updated_files,
            "updated_chunks": self.updated_chunks,
            "skipped_files": self.skipped_files,
            "deleted_chunks": self.deleted_chunks,
            "model": self.model,
            "store_path": self.store_path,
            "duration_ms": self.duration_ms,
            "incremental": self.incremental,
            "truncated": self.truncated,
            "timed_out": self.timed_out,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SearchSummary {
    pub(crate) query: String,
    pub(crate) model: String,
    pub(crate) count: usize,
    pub(crate) results: Vec<SearchResult>,
    pub(crate) timed_out: bool,
    pub(crate) index_status: String,
}

impl SearchSummary {
    pub(crate) fn into_payload(self) -> Value {
        let text = if self.results.is_empty() {
            "No semantic matches found.".to_string()
        } else {
            let estimated_text_len = self
                .results
                .iter()
                .map(|result| {
                    result.path.len()
                        + result.symbol.as_deref().map(str::len).unwrap_or_default()
                        + 40
                })
                .sum::<usize>();
            let mut text = String::with_capacity(estimated_text_len);
            for (index, result) in self.results.iter().enumerate() {
                if index > 0 {
                    text.push('\n');
                }
                let _ = write!(
                    text,
                    "{}:{}-{} {:.4}",
                    result.path, result.start_line, result.end_line, result.score
                );
                if let Some(symbol) = &result.symbol {
                    text.push(' ');
                    text.push_str(symbol);
                }
            }
            text
        };

        json!({
            "content": [{"type": "text", "text": text}],
            "isError": false,
            "query": self.query,
            "model": self.model,
            "count": self.count,
            "results": self.results,
            "timed_out": self.timed_out,
            "index_status": self.index_status,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SearchResult {
    pub(crate) chunk_id: String,
    pub(crate) path: String,
    pub(crate) language: String,
    pub(crate) symbol: Option<String>,
    pub(crate) start_line: u64,
    pub(crate) end_line: u64,
    pub(crate) score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content: Option<String>,
}

pub(crate) async fn index_workspace(options: IndexOptions) -> Result<IndexSummary> {
    let started = Instant::now();
    let deadline = started + Duration::from_millis(options.timeout_ms);
    let discovery = discover_files(DiscoveryOptions {
        path: options.path,
        hidden: options.hidden,
        no_ignore: options.no_ignore,
        limit: options.limit,
        timeout_ms: options.timeout_ms,
    })?;
    let scope = discovery.scope;
    let model_id = default_model_id();
    let model_slug = default_model_slug();
    let backend = SemanticBackend::from_env()?;
    let manifest_file = backend.manifest_file();
    let mut manifest = load_manifest(
        backend,
        &scope.index_dir,
        model_slug,
        manifest_file,
        &scope.workspace,
        model_id,
    )?;
    let discovered = discovered_path_set(&discovery.files);
    let stale_paths = manifest.stale_paths_under(&scope.target_filter, &discovered);

    let mut files_to_index = Vec::new();
    let mut skipped_files = discovery.skipped_files;
    for file in discovery.files {
        ensure_deadline(deadline)?;
        let bytes = tokio::fs::read(&file.absolute_path)
            .await
            .with_context(|| format!("failed to read {}", file.absolute_path.display()))?;
        let file_hash = hash_bytes(&bytes);
        if !options.force && manifest.is_current(&file.relative_path, &file_hash) {
            continue;
        }
        let source = match String::from_utf8(bytes) {
            Ok(source) => source,
            Err(_) => {
                skipped_files = skipped_files.saturating_add(1);
                continue;
            }
        };
        let chunks = chunk_source(&file, &source, &file_hash);
        if chunks.is_empty() {
            skipped_files = skipped_files.saturating_add(1);
            continue;
        }
        files_to_index.push((file.relative_path, file_hash, chunks));
    }

    let total_chunks = files_to_index
        .iter()
        .map(|(_, _, chunks)| chunks.len())
        .sum::<usize>();

    let mut updated_files = 0usize;
    let mut updated_chunks = 0usize;
    let mut deleted_chunks = manifest.chunk_count_for_paths(stale_paths.iter());

    if total_chunks == 0 {
        // Nothing changed and nothing went stale: skip the store round trip and the manifest
        // rewrite entirely. Both would be byte-identical no-ops.
        if !stale_paths.is_empty() {
            if let (Some(table), Some(dim)) = (manifest.table_name.clone(), manifest.vector_dim) {
                let store =
                    SemanticStore::open_existing(backend, &scope.index_dir, &table, dim).await?;
                store
                    .delete_paths(&workspace_key(&scope), &stale_paths)
                    .await
                    .context("failed to delete stale semantic chunks")?;
            }
            manifest.remove_paths(stale_paths);
            save_manifest(
                &manifest,
                backend,
                &scope.index_dir,
                model_slug,
                manifest_file,
            )?;
        }
        let (indexed_files, indexed_chunks) = indexed_counts_under(&manifest, &scope.target_filter);
        let store_path = manifest
            .table_name
            .as_deref()
            .map(|name| backend.store_location(&scope.index_dir, name))
            .unwrap_or_else(|| backend.store_location(&scope.index_dir, ""));
        return Ok(IndexSummary {
            indexed_files,
            indexed_chunks,
            updated_files,
            updated_chunks,
            skipped_files,
            deleted_chunks,
            model: model_id.to_string(),
            store_path,
            duration_ms: started.elapsed().as_millis(),
            incremental: !options.force,
            truncated: discovery.truncated,
            timed_out: discovery.timed_out,
        });
    }

    ensure_deadline(deadline)?;
    let provider = FastEmbedProvider::new(&scope.index_dir).await?;
    ensure_deadline(deadline)?;
    let embeddings = embed_index_chunks(&provider, &files_to_index, total_chunks, deadline).await?;
    let vector_dim = embeddings
        .first()
        .map(Vec::len)
        .ok_or_else(|| anyhow!("FastEmbed returned no document embeddings"))?;
    let index_name = backend.index_name(provider.model_slug(), vector_dim);
    let store =
        SemanticStore::open_or_create(backend, &scope.index_dir, provider.model_slug(), vector_dim)
            .await?;
    let root = workspace_key(&scope);

    let changed_paths = files_to_index
        .iter()
        .map(|(path, _, _)| path.clone())
        .chain(stale_paths.iter().cloned())
        .collect::<Vec<_>>();
    deleted_chunks = manifest.chunk_count_for_paths(changed_paths.iter());
    store
        .delete_paths(&root, &changed_paths)
        .await
        .context("failed to delete replaced semantic chunks")?;

    let indexed_at = Utc::now().to_rfc3339();
    let indexed_at_record = Arc::<str>::from(indexed_at.as_str());
    let root_record = Arc::<str>::from(root.as_str());
    let model_id_record = Arc::<str>::from(provider.model_id());
    let mut embedding_iter = embeddings.into_iter();
    let mut records = Vec::with_capacity(total_chunks);
    let mut manifest_updates = Vec::with_capacity(files_to_index.len());
    for (path, file_hash, chunks) in files_to_index {
        let chunk_ids = chunks
            .iter()
            .map(|chunk| chunk.chunk_id.clone())
            .collect::<Vec<_>>();
        for chunk in chunks {
            let embedding = embedding_iter
                .next()
                .ok_or_else(|| anyhow!("missing semantic embedding for indexed chunk"))?;
            records.push(StoredChunk {
                chunk,
                embedding,
                root: root_record.clone(),
                model_id: model_id_record.clone(),
                indexed_at: indexed_at_record.clone(),
            });
        }
        manifest_updates.push((path, file_hash, chunk_ids));
    }
    if embedding_iter.next().is_some() {
        bail!("FastEmbed returned more document embeddings than requested");
    }
    updated_chunks = records.len();
    store.add_chunks(records).await?;

    for (path, file_hash, chunk_ids) in manifest_updates {
        updated_files = updated_files.saturating_add(1);
        manifest.files.insert(
            path,
            ManifestFile {
                file_hash,
                chunk_ids,
                indexed_at: indexed_at.clone(),
            },
        );
    }
    manifest.remove_paths(stale_paths);
    manifest.table_name = Some(index_name.clone());
    manifest.vector_dim = Some(vector_dim);
    save_manifest(
        &manifest,
        backend,
        &scope.index_dir,
        provider.model_slug(),
        manifest_file,
    )?;
    let (indexed_files, indexed_chunks) = indexed_counts_under(&manifest, &scope.target_filter);

    Ok(IndexSummary {
        indexed_files,
        indexed_chunks,
        updated_files,
        updated_chunks,
        skipped_files,
        deleted_chunks,
        model: provider.model_id().to_string(),
        store_path: backend.store_location(&scope.index_dir, &index_name),
        duration_ms: started.elapsed().as_millis(),
        incremental: !options.force,
        truncated: discovery.truncated,
        timed_out: discovery.timed_out,
    })
}

pub(crate) async fn search_workspace(options: SearchOptions) -> Result<SearchSummary> {
    let started = Instant::now();
    let deadline = started + Duration::from_millis(options.timeout_ms);
    let scope = crate::discovery::resolve_scope(&options.path)?;
    let model_id = default_model_id();
    let model_slug = default_model_slug();
    let backend = SemanticBackend::from_env()?;
    let manifest_file = backend.manifest_file();
    let manifest = load_manifest(
        backend,
        &scope.index_dir,
        model_slug,
        manifest_file,
        &scope.workspace,
        model_id,
    )?;
    let index_name = manifest
        .table_name
        .clone()
        .ok_or_else(|| anyhow!("semantic index is empty for model {model_id}"))?;
    let vector_dim = manifest
        .vector_dim
        .ok_or_else(|| anyhow!("semantic index has no recorded vector dimension"))?;

    ensure_deadline(deadline)?;
    let provider = FastEmbedProvider::new(&scope.index_dir).await?;
    ensure_deadline(deadline)?;
    let query_embedding = provider.embed_query(&options.query).await?;
    if query_embedding.len() != vector_dim {
        bail!(
            "semantic query embedding dimension {} does not match index dimension {}",
            query_embedding.len(),
            vector_dim
        );
    }

    let store =
        SemanticStore::open_existing(backend, &scope.index_dir, &index_name, vector_dim).await?;
    let matches = store
        .search(
            query_embedding,
            SearchFilter {
                root: workspace_key(&scope),
                path_filter: scope.target_filter,
                language: options
                    .language
                    .map(|language| language.trim().to_ascii_lowercase()),
                limit: options.limit,
                threshold: options.threshold,
                include_content: options.include_content,
            },
        )
        .await?;

    Ok(SearchSummary {
        query: options.query,
        model: provider.model_id().to_string(),
        count: matches.len(),
        results: matches.into_iter().map(SearchResult::from).collect(),
        timed_out: started.elapsed() >= Duration::from_millis(options.timeout_ms),
        index_status: "ready".to_string(),
    })
}

fn embedding_document(chunk: &CodeChunk) -> String {
    let symbol_len = chunk
        .symbol
        .as_deref()
        .map(|symbol| "symbol: \n".len() + symbol.len())
        .unwrap_or_default();
    let mut document = String::with_capacity(
        "passage: ".len()
            + "path: \nlanguage: \ncode:\n".len()
            + chunk.path.len()
            + chunk.language.len()
            + symbol_len
            + chunk.content.len(),
    );
    document.push_str("path: ");
    document.push_str(&chunk.path);
    document.push_str("\nlanguage: ");
    document.push_str(&chunk.language);
    document.push('\n');
    if let Some(symbol) = &chunk.symbol {
        document.push_str("symbol: ");
        document.push_str(symbol);
        document.push('\n');
    }
    document.push_str("code:\n");
    document.push_str(&chunk.content);
    document
}

async fn embed_index_chunks(
    provider: &FastEmbedProvider,
    files_to_index: &[(String, String, Vec<CodeChunk>)],
    total_chunks: usize,
    deadline: Instant,
) -> Result<Vec<Vec<f32>>> {
    let batch_size = default_embedding_batch_size();
    let mut embeddings = Vec::with_capacity(total_chunks);
    let mut documents = Vec::with_capacity(batch_size);
    let mut vector_dim = None;

    for chunk in files_to_index
        .iter()
        .flat_map(|(_, _, chunks)| chunks.iter())
    {
        ensure_deadline(deadline)?;
        documents.push(embedding_document(chunk));
        if documents.len() >= batch_size {
            append_embedding_batch(
                provider,
                &mut documents,
                &mut embeddings,
                &mut vector_dim,
                batch_size,
            )
            .await?;
            ensure_deadline(deadline)?;
        }
    }

    if !documents.is_empty() {
        append_embedding_batch(
            provider,
            &mut documents,
            &mut embeddings,
            &mut vector_dim,
            batch_size,
        )
        .await?;
        ensure_deadline(deadline)?;
    }

    Ok(embeddings)
}

async fn append_embedding_batch(
    provider: &FastEmbedProvider,
    documents: &mut Vec<String>,
    embeddings: &mut Vec<Vec<f32>>,
    vector_dim: &mut Option<usize>,
    batch_size: usize,
) -> Result<()> {
    let expected = documents.len();
    let batch_documents = std::mem::take(documents);
    let batch_embeddings = provider.embed_documents(batch_documents).await?;
    documents.reserve(batch_size);

    if batch_embeddings.len() != expected {
        bail!(
            "FastEmbed returned {} document embeddings for {expected} input document(s)",
            batch_embeddings.len()
        );
    }

    for embedding in batch_embeddings {
        let actual_dim = embedding.len();
        if actual_dim == 0 {
            bail!("FastEmbed returned an empty document embedding");
        }
        match *vector_dim {
            Some(expected_dim) if expected_dim != actual_dim => {
                bail!(
                    "FastEmbed returned inconsistent document dimensions: expected {expected_dim}, got {actual_dim}"
                );
            }
            Some(_) => {}
            None => *vector_dim = Some(actual_dim),
        }
        embeddings.push(embedding);
    }

    Ok(())
}

fn workspace_key(scope: &WorkspaceScope) -> String {
    scope.workspace.display().to_string()
}

fn load_manifest(
    backend: SemanticBackend,
    index_dir: &std::path::Path,
    model_slug: &str,
    manifest_file: &str,
    workspace: &std::path::Path,
    model_id: &str,
) -> Result<IndexManifest> {
    match backend {
        SemanticBackend::LanceDb => {
            IndexManifest::load_or_new(index_dir, model_slug, workspace, model_id)
        }
        SemanticBackend::Qdrant => IndexManifest::load_or_new_named(
            index_dir,
            model_slug,
            manifest_file,
            workspace,
            model_id,
        ),
    }
}

fn save_manifest(
    manifest: &IndexManifest,
    backend: SemanticBackend,
    index_dir: &std::path::Path,
    model_slug: &str,
    manifest_file: &str,
) -> Result<()> {
    match backend {
        SemanticBackend::LanceDb => manifest.save(index_dir, model_slug),
        SemanticBackend::Qdrant => manifest.save_named(index_dir, model_slug, manifest_file),
    }
}

fn indexed_counts_under(manifest: &IndexManifest, filter: &PathFilter) -> (usize, usize) {
    manifest
        .files
        .iter()
        .filter(|(path, _)| filter.contains(path.as_str()))
        .fold((0usize, 0usize), |(files, chunks), (_, file)| {
            (
                files.saturating_add(1),
                chunks.saturating_add(file.chunk_ids.len()),
            )
        })
}

fn ensure_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        bail!("semantic operation timed out");
    }
    Ok(())
}

impl From<SemanticMatch> for SearchResult {
    fn from(value: SemanticMatch) -> Self {
        Self {
            chunk_id: value.chunk_id,
            path: value.path,
            language: value.language,
            symbol: value.symbol,
            start_line: value.start_line,
            end_line: value.end_line,
            score: value.score,
            content: value.content,
        }
    }
}

#[allow(dead_code)]
fn _relative_target_for_tests(scope: &WorkspaceScope) -> Result<String> {
    storage_relative_path(&scope.workspace, &scope.target)
}

#[cfg(test)]
mod tests {
    use super::{IndexSummary, SearchResult, SearchSummary, embedding_document};
    use crate::chunking::CodeChunk;

    #[test]
    fn index_payload_reports_indexed_and_updated_counts() {
        let payload = IndexSummary {
            indexed_files: 10,
            indexed_chunks: 42,
            updated_files: 2,
            updated_chunks: 7,
            skipped_files: 3,
            deleted_chunks: 4,
            model: "test-model".to_string(),
            store_path: "C:/repo/.tools-mcp/semantic-index".to_string(),
            duration_ms: 25,
            incremental: true,
            truncated: false,
            timed_out: false,
        }
        .into_payload();

        assert_eq!(payload["isError"], false);
        assert_eq!(payload["indexed_files"], 10);
        assert_eq!(payload["indexed_chunks"], 42);
        assert_eq!(payload["updated_files"], 2);
        assert_eq!(payload["updated_chunks"], 7);
        assert_eq!(payload["skipped_files"], 3);
        assert_eq!(
            payload["content"][0]["text"],
            "Indexed 10 file(s), updated 2 file(s); 42 chunk(s) indexed, 7 chunk(s) written; removed 4 stale/replaced chunk(s)."
        );
        assert!(
            !payload["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .contains("skipped")
        );
    }

    #[test]
    fn embedding_document_includes_stable_code_metadata() {
        let chunk = CodeChunk {
            chunk_id: "chunk-1".to_string(),
            path: "src/lib.rs".to_string(),
            language: "rust".to_string(),
            symbol: Some("register_tools".to_string()),
            start_line: 10,
            end_line: 12,
            content: "pub fn register_tools() {}".to_string(),
            content_hash: "content-hash".to_string(),
            file_hash: "file-hash".to_string(),
        };

        assert_eq!(
            embedding_document(&chunk),
            "path: src/lib.rs\nlanguage: rust\nsymbol: register_tools\ncode:\npub fn register_tools() {}"
        );
    }

    #[test]
    fn search_payload_preserves_contract_without_content() {
        let payload = SearchSummary {
            query: "register tool".to_string(),
            model: "test-model".to_string(),
            count: 1,
            results: vec![SearchResult {
                chunk_id: "chunk-1".to_string(),
                path: "src/lib.rs".to_string(),
                language: "rust".to_string(),
                symbol: Some("register_tools".to_string()),
                start_line: 10,
                end_line: 12,
                score: 0.25,
                content: None,
            }],
            timed_out: false,
            index_status: "ready".to_string(),
        }
        .into_payload();

        assert_eq!(payload["isError"], false);
        assert_eq!(payload["count"], 1);
        assert_eq!(
            payload["content"][0]["text"],
            "src/lib.rs:10-12 0.2500 register_tools"
        );
        assert!(payload["results"][0].get("content").is_none());
    }
}
