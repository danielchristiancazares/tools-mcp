use anyhow::Result;
use reqwest::Client;

use crate::files::upload_file;
use crate::openai::types::ResponsesCreate;
use crate::vector_stores::{
    add_file_to_vector_store_with_response, create_vector_store, wait_for_vector_store_file_ready,
};
use crate::{ApiConfig, BASE_URL};

/// Executes a semantic file search query against a vector store.
pub async fn responses_with_file_search(
    client: &Client,
    cfg: &ApiConfig,
    model: &str,
    query: &str,
    vector_store_id: &str,
    max_num_results: Option<u32>,
    include_results: bool,
) -> Result<serde_json::Value> {
    let url = format!("{BASE_URL}/responses");
    let mut tool =
        serde_json::json!({ "type": "file_search", "vector_store_ids": [vector_store_id] });
    if let Some(max_results) = max_num_results {
        tool["max_num_results"] = serde_json::json!(max_results);
    }
    let include = if include_results {
        Some(vec!["file_search_call.results"])
    } else {
        None
    };
    let body = ResponsesCreate {
        model,
        input: query,
        tools: vec![tool],
        include,
    };
    let response = client
        .post(url)
        .bearer_auth(&cfg.api_key)
        .header("OpenAI-Beta", "assistants=v2")
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    Ok(response.json().await?)
}

/// Convenience helper for one-off file search runs.
pub async fn file_search_run(
    client: &Client,
    cfg: &ApiConfig,
    file_path_or_url: &str,
    query: &str,
    model: Option<&str>,
    max_num_results: Option<u32>,
    include_results: bool,
) -> Result<serde_json::Value> {
    let file_id = upload_file(client, cfg, file_path_or_url).await?;
    let vector_store_id = create_vector_store(client, cfg, "knowledge_base").await?;
    let vector_store_file =
        add_file_to_vector_store_with_response(client, cfg, &vector_store_id, &file_id, None, None)
            .await?;
    wait_for_vector_store_file_ready(
        client,
        cfg,
        &vector_store_id,
        &vector_store_file.id,
        1000,
        60_000,
    )
    .await?;
    let response = responses_with_file_search(
        client,
        cfg,
        model.unwrap_or(&cfg.default_model),
        query,
        &vector_store_id,
        max_num_results,
        include_results,
    )
    .await?;
    Ok(serde_json::json!({
        "file_id": file_id,
        "vector_store_id": vector_store_id,
        "response": response
    }))
}
