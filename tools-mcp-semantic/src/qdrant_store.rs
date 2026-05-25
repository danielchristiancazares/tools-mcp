use crate::discovery::PathFilter;
use crate::store::{SearchFilter, SemanticMatch, StoredChunk};
use anyhow::{Context, Result, anyhow, bail};
use qdrant_client::qdrant::{
    Condition, CreateCollectionBuilder, CreateFieldIndexCollectionBuilder, DeletePointsBuilder,
    Distance, FieldType, Filter, PayloadSchemaInfo, PayloadSchemaType, PointStruct,
    QueryPointsBuilder, UpsertPointsBuilder, VectorParamsBuilder,
};
use qdrant_client::{Payload, Qdrant};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::env;
use std::time::Duration;
use url::Url;
use uuid::Uuid;

const COLLECTION_PREFIX: &str = "tools_mcp_semantic_chunks_v1";
const QDRANT_URL_ENV: &str = "QDRANT_URL";
const QDRANT_API_KEY_ENV: &str = "QDRANT_API_KEY";
const DEFAULT_QDRANT_GRPC_PORT: u16 = 6334;
const QDRANT_CLIENT_TIMEOUT: Duration = Duration::from_secs(30);
const QDRANT_DELETE_PATH_BATCH_SIZE: usize = 128;
const QDRANT_FIELD_INDEX_TIMEOUT_SECS: u64 = 30;
const QDRANT_FILTER_INDEX_FIELDS: &[&str] = &["root", "path", "path_prefixes", "language"];
const QDRANT_UPSERT_CHUNK_SIZE: usize = 128;

#[derive(Debug, Deserialize)]
struct QdrantChunkPayload {
    chunk_id: String,
    path: String,
    language: String,
    symbol: Option<String>,
    start_line: u64,
    end_line: u64,
    content: String,
}

pub(crate) struct QdrantStore {
    client: Qdrant,
    collection_name: String,
}

impl QdrantStore {
    pub(crate) async fn open_or_create(collection_name: String, vector_dim: usize) -> Result<Self> {
        let client = connect().await?;
        if !client
            .collection_exists(collection_name.as_str())
            .await
            .with_context(|| {
                format!("failed to check Qdrant collection {collection_name} existence")
            })?
        {
            client
                .create_collection(
                    CreateCollectionBuilder::new(collection_name.as_str()).vectors_config(
                        VectorParamsBuilder::new(vector_dim as u64, Distance::Cosine),
                    ),
                )
                .await
                .with_context(|| format!("failed to create Qdrant collection {collection_name}"))?;
        }
        ensure_filter_indexes(&client, collection_name.as_str()).await?;

        Ok(Self {
            client,
            collection_name,
        })
    }

    pub(crate) async fn open_existing(collection_name: String) -> Result<Self> {
        let client = connect().await?;
        if !client
            .collection_exists(collection_name.as_str())
            .await
            .with_context(|| {
                format!("failed to check Qdrant collection {collection_name} existence")
            })?
        {
            bail!("semantic Qdrant collection {collection_name} does not exist");
        }
        ensure_filter_indexes(&client, collection_name.as_str()).await?;

        Ok(Self {
            client,
            collection_name,
        })
    }

    pub(crate) async fn delete_paths(&self, root: &str, paths: &[String]) -> Result<usize> {
        if paths.is_empty() {
            return Ok(0);
        }

        for path_batch in paths.chunks(QDRANT_DELETE_PATH_BATCH_SIZE) {
            self.client
                .delete_points(
                    DeletePointsBuilder::new(self.collection_name.as_str())
                        .points(delete_paths_filter(root, path_batch))
                        .wait(true),
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to delete Qdrant semantic chunks for {} path(s)",
                        path_batch.len()
                    )
                })?;
        }

        Ok(paths.len())
    }

    pub(crate) async fn add_chunks(&self, records: Vec<StoredChunk>) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }

        let points = records
            .into_iter()
            .map(point_from_record)
            .collect::<Result<Vec<_>>>()?;
        self.client
            .upsert_points_chunked(
                UpsertPointsBuilder::new(self.collection_name.as_str(), points).wait(true),
                QDRANT_UPSERT_CHUNK_SIZE,
            )
            .await
            .context("failed to upsert semantic chunks to Qdrant")?;

        Ok(())
    }

    pub(crate) async fn search(
        &self,
        query_embedding: Vec<f32>,
        filter: SearchFilter,
    ) -> Result<Vec<SemanticMatch>> {
        let response = self
            .client
            .query(
                QueryPointsBuilder::new(self.collection_name.as_str())
                    .query(query_embedding)
                    .filter(search_filter(&filter))
                    .with_payload(true)
                    .limit(qdrant_search_limit(filter.limit)),
            )
            .await
            .context("failed to execute semantic vector query against Qdrant")?;

        let mut matches = Vec::with_capacity(filter.limit);
        for point in response.result {
            let Some(item) = match_from_point(point, &filter)? else {
                continue;
            };
            matches.push(item);
            if matches.len() >= filter.limit {
                break;
            }
        }

        Ok(matches)
    }
}

pub(crate) fn collection_name(model_slug: &str, vector_dim: usize) -> String {
    format!("{COLLECTION_PREFIX}_{model_slug}_{vector_dim}")
}

async fn connect() -> Result<Qdrant> {
    let url = configured_qdrant_url()?;
    let mut builder = qdrant_builder(&url);
    if let Some(api_key) = configured_qdrant_api_key()? {
        builder = builder.api_key(api_key);
    }
    builder
        .build()
        .context("failed to create Qdrant semantic client")
}

fn qdrant_builder(url: &str) -> qdrant_client::QdrantBuilder {
    Qdrant::from_url(url)
        .timeout(QDRANT_CLIENT_TIMEOUT)
        .skip_compatibility_check()
}

async fn ensure_filter_indexes(client: &Qdrant, collection_name: &str) -> Result<()> {
    let response = client
        .collection_info(collection_name)
        .await
        .with_context(|| format!("failed to inspect Qdrant collection {collection_name} schema"))?;
    let collection_info = response
        .result
        .with_context(|| format!("Qdrant collection {collection_name} info response was empty"))?;

    for field in missing_filter_index_fields(&collection_info.payload_schema)? {
        client
            .create_field_index(
                CreateFieldIndexCollectionBuilder::new(collection_name, field, FieldType::Keyword)
                    .wait(true)
                    .timeout(QDRANT_FIELD_INDEX_TIMEOUT_SECS),
            )
            .await
            .with_context(|| {
                format!("failed to create Qdrant keyword payload index for {field:?}")
            })?;
    }

    Ok(())
}

fn missing_filter_index_fields(
    payload_schema: &HashMap<String, PayloadSchemaInfo>,
) -> Result<Vec<&'static str>> {
    let mut missing = Vec::new();
    for field in QDRANT_FILTER_INDEX_FIELDS {
        let Some(schema) = payload_schema.get(*field) else {
            missing.push(*field);
            continue;
        };

        let data_type =
            PayloadSchemaType::try_from(schema.data_type).unwrap_or(PayloadSchemaType::UnknownType);
        match data_type {
            PayloadSchemaType::Keyword => {}
            PayloadSchemaType::UnknownType => missing.push(*field),
            _ => bail!(
                "Qdrant payload index for {field:?} has incompatible type {data_type:?}; expected keyword"
            ),
        }
    }
    Ok(missing)
}

fn configured_qdrant_url() -> Result<String> {
    let raw = env::var(QDRANT_URL_ENV)
        .with_context(|| format!("{QDRANT_URL_ENV} is required when using the Qdrant backend"))?;
    normalize_qdrant_url(&raw)
}

fn configured_qdrant_api_key() -> Result<Option<String>> {
    match env::var(QDRANT_API_KEY_ENV) {
        Ok(value) if value.trim().is_empty() => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn normalize_qdrant_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("{QDRANT_URL_ENV} must not be empty");
    }

    let mut url = Url::parse(trimmed)
        .with_context(|| format!("{QDRANT_URL_ENV} must be an absolute http(s) URL"))?;
    match url.scheme() {
        "http" | "https" => {}
        scheme => bail!("{QDRANT_URL_ENV} must use http or https, got {scheme:?}"),
    }
    if url.host_str().is_none() {
        bail!("{QDRANT_URL_ENV} must include a host");
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        bail!("{QDRANT_URL_ENV} must not include a path, query, or fragment");
    }
    if url.port().is_none() {
        url.set_port(Some(DEFAULT_QDRANT_GRPC_PORT))
            .map_err(|()| anyhow!("failed to add Qdrant gRPC port to {QDRANT_URL_ENV}"))?;
    }

    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn point_from_record(record: StoredChunk) -> Result<PointStruct> {
    let point_id = qdrant_point_id(&record.chunk.chunk_id)?;
    let payload = Payload::try_from(json!({
        "chunk_id": record.chunk.chunk_id,
        "root": record.root.as_ref(),
        "path": record.chunk.path,
        "path_prefixes": path_prefixes(&record.chunk.path),
        "language": record.chunk.language,
        "symbol": record.chunk.symbol,
        "start_line": record.chunk.start_line,
        "end_line": record.chunk.end_line,
        "content": record.chunk.content,
        "content_hash": record.chunk.content_hash,
        "file_hash": record.chunk.file_hash,
        "model_id": record.model_id.as_ref(),
        "indexed_at": record.indexed_at.as_ref(),
    }))
    .context("failed to build Qdrant semantic payload")?;

    Ok(PointStruct::new(point_id, record.embedding, payload))
}

fn qdrant_point_id(chunk_id: &str) -> Result<String> {
    let bytes = hex::decode(chunk_id).context("semantic chunk id is not valid hex")?;
    let uuid_bytes: [u8; 16] = bytes
        .get(..16)
        .ok_or_else(|| anyhow!("semantic chunk id is too short for a Qdrant UUID"))?
        .try_into()
        .expect("slice length checked");
    Ok(Uuid::from_bytes(uuid_bytes).hyphenated().to_string())
}

fn path_prefixes(path: &str) -> Vec<String> {
    let mut prefixes = Vec::new();
    let mut offset = 0usize;
    while let Some(index) = path[offset..].find('/') {
        let end = offset + index;
        if end > 0 {
            prefixes.push(path[..end].to_string());
        }
        offset = end + 1;
    }
    prefixes
}

fn delete_paths_filter(root: &str, paths: &[String]) -> Filter {
    let mut must = Vec::with_capacity(2);
    must.push(Condition::matches("root", root.to_string()));
    if let [path] = paths {
        must.push(Condition::matches("path", path.clone()));
    } else {
        must.push(
            Filter::any(
                paths
                    .iter()
                    .map(|path| Condition::matches("path", path.clone())),
            )
            .into(),
        );
    }
    Filter::must(must)
}

fn search_filter(filter: &SearchFilter) -> Filter {
    let mut must = Vec::with_capacity(3);
    must.push(Condition::matches("root", filter.root.clone()));
    match &filter.path_filter {
        PathFilter::Workspace => {}
        PathFilter::File(path) => must.push(Condition::matches("path", path.clone())),
        PathFilter::Directory(path) => {
            must.push(Condition::matches("path_prefixes", path.clone()));
        }
    }
    if let Some(language) = normalized_language(&filter.language) {
        must.push(Condition::matches("language", language));
    }
    Filter::must(must)
}

fn qdrant_search_limit(limit: usize) -> u64 {
    limit.saturating_mul(10).clamp(100, 1000) as u64
}

fn match_from_point(
    point: qdrant_client::qdrant::ScoredPoint,
    filter: &SearchFilter,
) -> Result<Option<SemanticMatch>> {
    let payload: QdrantChunkPayload = Payload::from(point.payload)
        .deserialize()
        .context("failed to deserialize semantic Qdrant payload")?;
    if !filter.path_filter.contains(&payload.path) {
        return Ok(None);
    }
    if let Some(language) = normalized_language(&filter.language)
        && payload.language != language
    {
        return Ok(None);
    }

    let distance = qdrant_cosine_score_to_distance(point.score);
    if filter
        .threshold
        .is_some_and(|threshold| distance > threshold)
    {
        return Ok(None);
    }

    Ok(Some(SemanticMatch {
        chunk_id: payload.chunk_id,
        path: payload.path,
        language: payload.language,
        symbol: payload.symbol,
        start_line: payload.start_line,
        end_line: payload.end_line,
        score: distance,
        content: filter.include_content.then_some(payload.content),
    }))
}

fn normalized_language(language: &Option<String>) -> Option<String> {
    language
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

fn qdrant_cosine_score_to_distance(score: f32) -> f32 {
    1.0 - score
}

#[cfg(test)]
mod tests {
    use super::{
        QDRANT_CLIENT_TIMEOUT, QDRANT_FILTER_INDEX_FIELDS, collection_name,
        missing_filter_index_fields, normalize_qdrant_url, path_prefixes, qdrant_builder,
        qdrant_cosine_score_to_distance, qdrant_point_id,
    };
    use qdrant_client::qdrant::{PayloadSchemaInfo, PayloadSchemaType};
    use std::collections::HashMap;

    #[test]
    fn qdrant_url_defaults_to_grpc_port() {
        assert_eq!(
            normalize_qdrant_url("https://example.cloud.qdrant.io").expect("normalize"),
            "https://example.cloud.qdrant.io:6334"
        );
    }

    #[test]
    fn qdrant_url_preserves_explicit_port() {
        assert_eq!(
            normalize_qdrant_url("http://localhost:6334").expect("normalize"),
            "http://localhost:6334"
        );
    }

    #[test]
    fn qdrant_collection_names_are_stable() {
        assert_eq!(
            collection_name("jinaai_jina_embeddings_v2_base_code", 768),
            "tools_mcp_semantic_chunks_v1_jinaai_jina_embeddings_v2_base_code_768"
        );
    }

    #[test]
    fn qdrant_builder_disables_stdout_compatibility_check() {
        let builder = qdrant_builder("https://example.cloud.qdrant.io:6334");

        assert_eq!(builder.timeout, QDRANT_CLIENT_TIMEOUT);
        assert!(!builder.check_compatibility);
    }

    #[test]
    fn qdrant_filter_index_fields_cover_delete_and_search_filters() {
        assert_eq!(
            QDRANT_FILTER_INDEX_FIELDS,
            &["root", "path", "path_prefixes", "language"]
        );
    }

    #[test]
    fn missing_filter_index_fields_reports_absent_indexes() {
        let mut payload_schema = HashMap::new();
        payload_schema.insert(
            "root".to_string(),
            payload_schema_info(PayloadSchemaType::Keyword),
        );

        assert_eq!(
            missing_filter_index_fields(&payload_schema).expect("missing fields"),
            vec!["path", "path_prefixes", "language"]
        );
    }

    #[test]
    fn missing_filter_index_fields_rejects_wrong_index_type() {
        let mut payload_schema = HashMap::new();
        for field in QDRANT_FILTER_INDEX_FIELDS {
            payload_schema.insert(
                (*field).to_string(),
                payload_schema_info(PayloadSchemaType::Keyword),
            );
        }
        payload_schema.insert(
            "path".to_string(),
            payload_schema_info(PayloadSchemaType::Text),
        );

        let err = missing_filter_index_fields(&payload_schema).expect_err("wrong index type");

        assert!(
            err.to_string()
                .contains("Qdrant payload index for \"path\" has incompatible type Text")
        );
    }

    #[test]
    fn qdrant_point_ids_are_deterministic_uuids() {
        assert_eq!(
            qdrant_point_id("00112233445566778899aabbccddeeff0011223344556677").expect("point id"),
            "00112233-4455-6677-8899-aabbccddeeff"
        );
    }

    #[test]
    fn path_prefixes_include_parent_directories() {
        assert_eq!(
            path_prefixes("tools-mcp-semantic/src/model.rs"),
            vec!["tools-mcp-semantic", "tools-mcp-semantic/src"]
        );
    }

    #[test]
    fn qdrant_scores_are_projected_as_distances() {
        assert_eq!(qdrant_cosine_score_to_distance(0.75), 0.25);
    }

    fn payload_schema_info(data_type: PayloadSchemaType) -> PayloadSchemaInfo {
        PayloadSchemaInfo {
            data_type: data_type as i32,
            params: None,
            points: None,
        }
    }
}
