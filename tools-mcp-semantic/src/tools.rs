use serde::Deserialize;
use serde_json::{Value, json};
use tools_mcp_core::{ToolCallOutcome, define_mcp_tool, validation};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SemanticIndexRequest {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    force: Option<bool>,
    #[serde(default)]
    hidden: Option<bool>,
    #[serde(default)]
    no_ignore: Option<bool>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SemanticSearchRequest {
    query: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    threshold: Option<f32>,
    #[serde(default)]
    include_content: Option<bool>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

pub fn register_tools(registry: &mut tools_mcp_core::ToolRegistry) {
    registry.register::<SemanticIndexTool>();
    registry.register::<SemanticSearchTool>();
}

async fn handle_semantic_index(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match ToolCallOutcome::parse_args::<SemanticIndexRequest>(&args) {
        Ok(req) => req,
        Err(outcome) => return outcome,
    };

    let path = req.path.unwrap_or_else(|| ".".to_string());
    if let Err(outcome) = validation::validate_non_empty(&path, "path", None) {
        return outcome;
    }

    match crate::model::index_workspace(crate::model::IndexOptions {
        path,
        force: req.force.unwrap_or(false),
        hidden: req.hidden.unwrap_or(false),
        no_ignore: req.no_ignore.unwrap_or(false),
        limit: validation::clamp_limit(req.limit, 10_000, 1, 100_000),
        timeout_ms: validation::clamp_timeout(req.timeout_ms, 300_000, 1_000, 1_800_000),
    })
    .await
    {
        Ok(summary) => ToolCallOutcome::ok(summary.into_payload()),
        Err(err) => ToolCallOutcome::err_with(
            format!("semantic index failed: {err}"),
            [(
                "remediation",
                json!("Check the path, local model availability, and index directory permissions."),
            )],
        ),
    }
}

async fn handle_semantic_search(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match ToolCallOutcome::parse_args::<SemanticSearchRequest>(&args) {
        Ok(req) => req,
        Err(outcome) => return outcome,
    };

    if let Err(outcome) = validation::validate_non_empty(&req.query, "query", None) {
        return outcome;
    }

    let path = req.path.unwrap_or_else(|| ".".to_string());
    if let Err(outcome) = validation::validate_non_empty(&path, "path", None) {
        return outcome;
    }

    match crate::model::search_workspace(crate::model::SearchOptions {
        query: req.query,
        path,
        limit: validation::clamp_limit(req.limit, 10, 1, 100),
        language: req.language,
        threshold: req.threshold,
        include_content: req.include_content.unwrap_or(true),
        timeout_ms: validation::clamp_timeout(req.timeout_ms, 60_000, 1_000, 300_000),
    })
    .await
    {
        Ok(results) => ToolCallOutcome::ok(results.into_payload()),
        Err(err) => ToolCallOutcome::err_with(
            format!("semantic search failed: {err}"),
            [(
                "remediation",
                json!("Run SemanticIndex for the target path, or check model/index compatibility."),
            )],
        ),
    }
}

define_mcp_tool! {
    SemanticIndexTool,
    name: "SemanticIndex",
    description: "Create or refresh a local semantic code-search index for a workspace path using local embeddings.",
    schema: {
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "File or directory to index, defaulting to the server working directory."},
            "force": {"type": "boolean", "default": false, "description": "Reindex files even when the stored file hash has not changed."},
            "hidden": {"type": "boolean", "default": false, "description": "Include hidden files and directories."},
            "no_ignore": {"type": "boolean", "default": false, "description": "Bypass ignore files such as .gitignore."},
            "limit": {"type": "integer", "minimum": 1, "maximum": 100000, "default": 10000, "description": "Maximum number of files to consider."},
            "timeout_ms": {"type": "integer", "minimum": 1000, "maximum": 1800000, "default": 300000, "description": "Indexing timeout budget in milliseconds."}
        },
        "additionalProperties": false
    },
    handler: handle_semantic_index
}

define_mcp_tool! {
    SemanticSearchTool,
    name: "SemanticSearch",
    description: "Search a local semantic code index with a natural-language query and return ranked code chunks.",
    schema: {
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": "Natural-language search query."},
            "path": {"type": "string", "description": "Indexed file or directory scope to search, defaulting to the server working directory."},
            "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 10, "description": "Maximum number of ranked chunks to return."},
            "language": {"type": "string", "description": "Optional language filter, for example rust, typescript, python, go, markdown."},
            "threshold": {"type": "number", "description": "Optional maximum vector distance threshold; lower is more similar."},
            "include_content": {"type": "boolean", "default": true, "description": "Include chunk content in structured results."},
            "timeout_ms": {"type": "integer", "minimum": 1000, "maximum": 300000, "default": 60000, "description": "Search timeout budget in milliseconds."}
        },
        "required": ["query"],
        "additionalProperties": false
    },
    handler: handle_semantic_search
}
