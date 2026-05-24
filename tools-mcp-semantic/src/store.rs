use crate::chunking::CodeChunk;
use crate::discovery::{PathFilter, escape_sql_literal};
use anyhow::{Context, Result, anyhow};
use arrow_array::builder::{FixedSizeListBuilder, Float32Builder, StringBuilder, UInt64Builder};
use arrow_array::{Array, Float32Array, Float64Array, RecordBatch, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use futures::TryStreamExt;
use lancedb::Table;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use std::path::Path;
use std::sync::Arc;

const TABLE_PREFIX: &str = "semantic_chunks_v1";
const DELETE_PATH_BATCH_SIZE: usize = 256;
const SEARCH_COLUMNS_WITH_CONTENT: [&str; 7] = [
    "chunk_id",
    "path",
    "language",
    "symbol",
    "start_line",
    "end_line",
    "content",
];
const SEARCH_COLUMNS_WITHOUT_CONTENT: [&str; 6] = [
    "chunk_id",
    "path",
    "language",
    "symbol",
    "start_line",
    "end_line",
];

#[derive(Clone, Debug)]
pub(crate) struct StoredChunk {
    pub(crate) chunk: CodeChunk,
    pub(crate) embedding: Vec<f32>,
    pub(crate) root: Arc<str>,
    pub(crate) model_id: Arc<str>,
    pub(crate) indexed_at: Arc<str>,
}

#[derive(Clone, Debug)]
pub(crate) struct SemanticMatch {
    pub(crate) chunk_id: String,
    pub(crate) path: String,
    pub(crate) language: String,
    pub(crate) symbol: Option<String>,
    pub(crate) start_line: u64,
    pub(crate) end_line: u64,
    pub(crate) score: f32,
    pub(crate) content: Option<String>,
}

pub(crate) struct LanceDbStore {
    table: Table,
    schema: SchemaRef,
}

pub(crate) struct SearchFilter {
    pub(crate) root: String,
    pub(crate) path_filter: PathFilter,
    pub(crate) language: Option<String>,
    pub(crate) limit: usize,
    pub(crate) threshold: Option<f32>,
    pub(crate) include_content: bool,
}

impl LanceDbStore {
    pub(crate) async fn open_or_create(
        index_dir: &Path,
        model_slug: &str,
        vector_dim: usize,
    ) -> Result<Self> {
        let table_name = table_name(model_slug, vector_dim);
        let schema = schema(vector_dim);
        let db = open_database(index_dir).await?;
        let table = match db.open_table(table_name.clone()).execute().await {
            Ok(table) => table,
            Err(_) => db
                .create_empty_table(table_name.clone(), schema.clone())
                .execute()
                .await
                .with_context(|| format!("failed to create LanceDB table {table_name}"))?,
        };

        Ok(Self { table, schema })
    }

    pub(crate) async fn open_existing(
        index_dir: &Path,
        table_name: &str,
        vector_dim: usize,
    ) -> Result<Self> {
        let db = open_database(index_dir).await?;
        let table = db
            .open_table(table_name.to_string())
            .execute()
            .await
            .with_context(|| format!("semantic index table {table_name} does not exist"))?;
        Ok(Self {
            table,
            schema: schema(vector_dim),
        })
    }

    pub(crate) async fn delete_paths(&self, root: &str, paths: &[String]) -> Result<usize> {
        if paths.is_empty() {
            return Ok(0);
        }

        for path_batch in paths.chunks(DELETE_PATH_BATCH_SIZE) {
            let predicate = delete_paths_predicate(root, path_batch);
            self.table.delete(&predicate).await.with_context(|| {
                format!(
                    "failed to delete stale semantic chunks for {} path(s)",
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

        let batch = records_to_batch(&records, self.schema.clone())?;
        self.table
            .add(vec![batch])
            .execute()
            .await
            .context("failed to add semantic chunks to LanceDB")?;
        Ok(())
    }

    pub(crate) async fn search(
        &self,
        query_embedding: Vec<f32>,
        filter: SearchFilter,
    ) -> Result<Vec<SemanticMatch>> {
        let predicate = build_filter_predicate(&filter);
        let mut query = self
            .table
            .query()
            .nearest_to(query_embedding.as_slice())
            .context("failed to configure semantic vector query")?
            .column("vector")
            .select(search_projection(filter.include_content))
            .limit(filter.limit);

        if let Some(predicate) = predicate {
            query = query.only_if(predicate);
        }

        let mut stream = query
            .execute()
            .await
            .context("failed to execute semantic vector query")?;

        let mut results = Vec::with_capacity(filter.limit);
        while let Some(batch) = stream
            .try_next()
            .await
            .context("failed to collect semantic vector query results")?
        {
            append_matches(&batch, &filter, &mut results)?;
        }
        Ok(results)
    }
}

pub(crate) fn table_name(model_slug: &str, vector_dim: usize) -> String {
    format!("{TABLE_PREFIX}_{model_slug}_{vector_dim}")
}

async fn open_database(index_dir: &Path) -> Result<lancedb::Connection> {
    let db_dir = index_dir.join("lancedb");
    tokio::fs::create_dir_all(&db_dir)
        .await
        .with_context(|| format!("failed to create LanceDB directory {}", db_dir.display()))?;
    let uri = db_dir.to_string_lossy().to_string();
    lancedb::connect(&uri)
        .execute()
        .await
        .context("failed to open LanceDB semantic index")
}

fn schema(vector_dim: usize) -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("chunk_id", DataType::Utf8, false),
        Field::new("root", DataType::Utf8, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("language", DataType::Utf8, false),
        Field::new("symbol", DataType::Utf8, true),
        Field::new("start_line", DataType::UInt64, false),
        Field::new("end_line", DataType::UInt64, false),
        Field::new("content", DataType::Utf8, false),
        Field::new("content_hash", DataType::Utf8, false),
        Field::new("file_hash", DataType::Utf8, false),
        Field::new("model_id", DataType::Utf8, false),
        Field::new("indexed_at", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                vector_dim as i32,
            ),
            true,
        ),
    ]))
}

fn records_to_batch(records: &[StoredChunk], schema: SchemaRef) -> Result<RecordBatch> {
    let row_count = records.len();
    let vector_dim = records
        .first()
        .map(|record| record.embedding.len())
        .ok_or_else(|| anyhow!("cannot create semantic batch without records"))?;
    if records
        .iter()
        .any(|record| record.embedding.len() != vector_dim)
    {
        return Err(anyhow!("semantic embedding dimensions are inconsistent"));
    }
    let vector_dim = i32::try_from(vector_dim)
        .context("semantic vector dimension exceeds Arrow fixed-size list limit")?;
    let vector_value_count = row_count
        .checked_mul(vector_dim as usize)
        .ok_or_else(|| anyhow!("semantic vector batch is too large"))?;
    let capacities = BatchStringCapacities::for_records(records);

    let mut chunk_ids = StringBuilder::with_capacity(row_count, capacities.chunk_ids);
    let mut roots = StringBuilder::with_capacity(row_count, capacities.roots);
    let mut paths = StringBuilder::with_capacity(row_count, capacities.paths);
    let mut languages = StringBuilder::with_capacity(row_count, capacities.languages);
    let mut symbols = StringBuilder::with_capacity(row_count, capacities.symbols);
    let mut start_lines = UInt64Builder::with_capacity(row_count);
    let mut end_lines = UInt64Builder::with_capacity(row_count);
    let mut contents = StringBuilder::with_capacity(row_count, capacities.contents);
    let mut content_hashes = StringBuilder::with_capacity(row_count, capacities.content_hashes);
    let mut file_hashes = StringBuilder::with_capacity(row_count, capacities.file_hashes);
    let mut model_ids = StringBuilder::with_capacity(row_count, capacities.model_ids);
    let mut indexed_at = StringBuilder::with_capacity(row_count, capacities.indexed_at);
    let mut vectors = FixedSizeListBuilder::with_capacity(
        Float32Builder::with_capacity(vector_value_count),
        vector_dim,
        row_count,
    );

    for record in records {
        chunk_ids.append_value(&record.chunk.chunk_id);
        roots.append_value(record.root.as_ref());
        paths.append_value(&record.chunk.path);
        languages.append_value(&record.chunk.language);
        symbols.append_option(record.chunk.symbol.as_deref());
        start_lines.append_value(record.chunk.start_line);
        end_lines.append_value(record.chunk.end_line);
        contents.append_value(&record.chunk.content);
        content_hashes.append_value(&record.chunk.content_hash);
        file_hashes.append_value(&record.chunk.file_hash);
        model_ids.append_value(record.model_id.as_ref());
        indexed_at.append_value(record.indexed_at.as_ref());
        for value in &record.embedding {
            vectors.values().append_value(*value);
        }
        vectors.append(true);
    }

    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(chunk_ids.finish()),
            Arc::new(roots.finish()),
            Arc::new(paths.finish()),
            Arc::new(languages.finish()),
            Arc::new(symbols.finish()),
            Arc::new(start_lines.finish()),
            Arc::new(end_lines.finish()),
            Arc::new(contents.finish()),
            Arc::new(content_hashes.finish()),
            Arc::new(file_hashes.finish()),
            Arc::new(model_ids.finish()),
            Arc::new(indexed_at.finish()),
            Arc::new(vectors.finish()),
        ],
    )
    .context("failed to create semantic LanceDB record batch")
}

fn delete_paths_predicate(root: &str, paths: &[String]) -> String {
    let root = escape_sql_literal(root);
    if let [path] = paths {
        return format!("root = '{root}' AND path = '{}'", escape_sql_literal(path));
    }

    let path_literals_len = paths.iter().map(|path| path.len() + 4).sum::<usize>();
    let mut predicate = String::with_capacity(root.len() + path_literals_len + 32);
    predicate.push_str("root = '");
    predicate.push_str(&root);
    predicate.push_str("' AND path IN (");
    for (index, path) in paths.iter().enumerate() {
        if index > 0 {
            predicate.push_str(", ");
        }
        predicate.push('\'');
        predicate.push_str(&escape_sql_literal(path));
        predicate.push('\'');
    }
    predicate.push(')');
    predicate
}

fn search_projection(include_content: bool) -> Select {
    if include_content {
        Select::columns(&SEARCH_COLUMNS_WITH_CONTENT)
    } else {
        Select::columns(&SEARCH_COLUMNS_WITHOUT_CONTENT)
    }
}

fn build_filter_predicate(filter: &SearchFilter) -> Option<String> {
    let mut clauses = vec![format!("root = '{}'", escape_sql_literal(&filter.root))];
    if let Some(path_filter) = filter.path_filter.to_sql() {
        clauses.push(path_filter);
    }
    if let Some(language) = filter
        .language
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        clauses.push(format!(
            "language = '{}'",
            escape_sql_literal(language.trim())
        ));
    }
    Some(clauses.join(" AND "))
}

fn append_matches(
    batch: &RecordBatch,
    filter: &SearchFilter,
    results: &mut Vec<SemanticMatch>,
) -> Result<()> {
    let chunk_ids = string_column(batch, "chunk_id")?;
    let paths = string_column(batch, "path")?;
    let languages = string_column(batch, "language")?;
    let symbols = string_column(batch, "symbol")?;
    let start_lines = uint64_column(batch, "start_line")?;
    let end_lines = uint64_column(batch, "end_line")?;
    let contents = filter
        .include_content
        .then(|| string_column(batch, "content"))
        .transpose()?;

    for row in 0..batch.num_rows() {
        let score = distance_value(batch, row).unwrap_or(0.0);
        if filter.threshold.is_some_and(|threshold| score > threshold) {
            continue;
        }

        results.push(SemanticMatch {
            chunk_id: chunk_ids.value(row).to_string(),
            path: paths.value(row).to_string(),
            language: languages.value(row).to_string(),
            symbol: (!symbols.is_null(row)).then(|| symbols.value(row).to_string()),
            start_line: start_lines.value(row),
            end_line: end_lines.value(row),
            score,
            content: contents.map(|contents| contents.value(row).to_string()),
        });
    }
    Ok(())
}

#[derive(Default)]
struct BatchStringCapacities {
    chunk_ids: usize,
    roots: usize,
    paths: usize,
    languages: usize,
    symbols: usize,
    contents: usize,
    content_hashes: usize,
    file_hashes: usize,
    model_ids: usize,
    indexed_at: usize,
}

impl BatchStringCapacities {
    fn for_records(records: &[StoredChunk]) -> Self {
        let mut capacities = Self::default();
        for record in records {
            capacities.chunk_ids += record.chunk.chunk_id.len();
            capacities.roots += record.root.len();
            capacities.paths += record.chunk.path.len();
            capacities.languages += record.chunk.language.len();
            capacities.symbols += record
                .chunk
                .symbol
                .as_ref()
                .map(String::len)
                .unwrap_or_default();
            capacities.contents += record.chunk.content.len();
            capacities.content_hashes += record.chunk.content_hash.len();
            capacities.file_hashes += record.chunk.file_hash.len();
            capacities.model_ids += record.model_id.len();
            capacities.indexed_at += record.indexed_at.len();
        }
        capacities
    }
}

fn string_column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    batch
        .column_by_name(name)
        .ok_or_else(|| anyhow!("semantic query result missing '{name}' column"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("semantic query result column '{name}' has unexpected type"))
}

fn uint64_column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a UInt64Array> {
    batch
        .column_by_name(name)
        .ok_or_else(|| anyhow!("semantic query result missing '{name}' column"))?
        .as_any()
        .downcast_ref::<UInt64Array>()
        .ok_or_else(|| anyhow!("semantic query result column '{name}' has unexpected type"))
}

fn distance_value(batch: &RecordBatch, row: usize) -> Option<f32> {
    let column = batch.column_by_name("_distance")?;
    if let Some(values) = column.as_any().downcast_ref::<Float32Array>() {
        return (!values.is_null(row)).then(|| values.value(row));
    }
    if let Some(values) = column.as_any().downcast_ref::<Float64Array>() {
        return (!values.is_null(row)).then(|| values.value(row) as f32);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        LanceDbStore, SearchFilter, StoredChunk, build_filter_predicate, delete_paths_predicate,
        table_name,
    };
    use crate::chunking::CodeChunk;
    use crate::discovery::PathFilter;
    use std::sync::Arc;

    #[test]
    fn table_names_include_model_and_dimension() {
        assert_eq!(
            table_name("jina_code", 768),
            "semantic_chunks_v1_jina_code_768"
        );
    }

    #[test]
    fn search_filter_combines_root_path_and_language() {
        let filter = SearchFilter {
            root: "C:/repo".to_string(),
            path_filter: PathFilter::Directory("src".to_string()),
            language: Some("rust".to_string()),
            limit: 10,
            threshold: None,
            include_content: true,
        };

        let predicate = build_filter_predicate(&filter).expect("predicate");
        assert!(predicate.contains("root = 'C:/repo'"));
        assert!(predicate.contains("path >= 'src/'"));
        assert!(predicate.contains("path < 'src0'"));
        assert!(predicate.contains("language = 'rust'"));
    }

    #[tokio::test]
    async fn directory_filter_matches_underscore_paths_literally() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LanceDbStore::open_or_create(temp.path(), "test_model", 2)
            .await
            .expect("open semantic store");
        store
            .add_chunks(vec![
                stored_chunk("smart_file_edit/src/lib.rs", vec![1.0, 0.0]),
                stored_chunk("smartXfile_edit/src/lib.rs", vec![0.0, 1.0]),
            ])
            .await
            .expect("add chunks");

        let matches = store
            .search(
                vec![1.0, 0.0],
                SearchFilter {
                    root: "repo".to_string(),
                    path_filter: PathFilter::Directory("smart_file_edit".to_string()),
                    language: None,
                    limit: 10,
                    threshold: None,
                    include_content: false,
                },
            )
            .await
            .expect("search chunks");

        let paths = matches
            .iter()
            .map(|item| item.path.clone())
            .collect::<Vec<_>>();
        assert_eq!(paths, vec!["smart_file_edit/src/lib.rs"]);
        assert!(matches.iter().all(|item| item.content.is_none()));
    }

    #[test]
    fn delete_paths_predicate_escapes_batched_literals() {
        assert_eq!(
            delete_paths_predicate(
                "repo's",
                &["src/it_was.rs".to_string(), "src/it's.rs".to_string()]
            ),
            "root = 'repo''s' AND path IN ('src/it_was.rs', 'src/it''s.rs')"
        );
    }

    #[tokio::test]
    async fn delete_paths_removes_multiple_escaped_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LanceDbStore::open_or_create(temp.path(), "test_model", 2)
            .await
            .expect("open semantic store");
        store
            .add_chunks(vec![
                stored_chunk("src/it_was.rs", vec![1.0, 0.0]),
                stored_chunk("src/it's.rs", vec![0.9, 0.1]),
                stored_chunk("src/keep.rs", vec![0.0, 1.0]),
            ])
            .await
            .expect("add chunks");

        let deleted = store
            .delete_paths(
                "repo",
                &["src/it_was.rs".to_string(), "src/it's.rs".to_string()],
            )
            .await
            .expect("delete paths");
        assert_eq!(deleted, 2);

        let matches = store
            .search(
                vec![1.0, 0.0],
                SearchFilter {
                    root: "repo".to_string(),
                    path_filter: PathFilter::Workspace,
                    language: None,
                    limit: 10,
                    threshold: None,
                    include_content: true,
                },
            )
            .await
            .expect("search chunks");

        let paths = matches
            .into_iter()
            .map(|item| item.path)
            .collect::<Vec<_>>();
        assert_eq!(paths, vec!["src/keep.rs"]);
    }

    #[tokio::test]
    async fn search_respects_content_projection_flag() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LanceDbStore::open_or_create(temp.path(), "test_model", 2)
            .await
            .expect("open semantic store");
        store
            .add_chunks(vec![stored_chunk("src/lib.rs", vec![1.0, 0.0])])
            .await
            .expect("add chunks");

        let without_content = store
            .search(
                vec![1.0, 0.0],
                SearchFilter {
                    root: "repo".to_string(),
                    path_filter: PathFilter::Workspace,
                    language: None,
                    limit: 10,
                    threshold: None,
                    include_content: false,
                },
            )
            .await
            .expect("search without content");
        assert_eq!(without_content[0].content, None);

        let with_content = store
            .search(
                vec![1.0, 0.0],
                SearchFilter {
                    root: "repo".to_string(),
                    path_filter: PathFilter::Workspace,
                    language: None,
                    limit: 10,
                    threshold: None,
                    include_content: true,
                },
            )
            .await
            .expect("search with content");
        assert_eq!(with_content[0].content.as_deref(), Some("fn sample() {}"));
    }

    #[tokio::test]
    async fn search_threshold_uses_projected_distance() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LanceDbStore::open_or_create(temp.path(), "test_model", 2)
            .await
            .expect("open semantic store");
        store
            .add_chunks(vec![
                stored_chunk("src/near.rs", vec![1.0, 0.0]),
                stored_chunk("src/far.rs", vec![0.0, 1.0]),
            ])
            .await
            .expect("add chunks");

        let matches = store
            .search(
                vec![1.0, 0.0],
                SearchFilter {
                    root: "repo".to_string(),
                    path_filter: PathFilter::Workspace,
                    language: None,
                    limit: 10,
                    threshold: Some(0.1),
                    include_content: false,
                },
            )
            .await
            .expect("search chunks");

        let paths = matches
            .into_iter()
            .map(|item| item.path)
            .collect::<Vec<_>>();
        assert_eq!(paths, vec!["src/near.rs"]);
    }

    fn stored_chunk(path: &str, embedding: Vec<f32>) -> StoredChunk {
        StoredChunk {
            chunk: CodeChunk {
                chunk_id: path.replace('/', "-"),
                path: path.to_string(),
                language: "rust".to_string(),
                symbol: None,
                start_line: 1,
                end_line: 1,
                content: "fn sample() {}".to_string(),
                content_hash: format!("{path}-content"),
                file_hash: format!("{path}-file"),
            },
            embedding,
            root: Arc::from("repo"),
            model_id: Arc::from("test_model"),
            indexed_at: Arc::from("2026-05-22T00:00:00Z"),
        }
    }
}
