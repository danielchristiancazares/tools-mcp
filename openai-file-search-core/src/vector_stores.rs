use anyhow::{Context, Result};
use reqwest::Client;
use tokio::time::{Duration, sleep};

use crate::openai::types::{VectorStoreCreate, VectorStoreFileCreate};
use crate::{
    ApiConfig, BASE_URL, FileInfo, VectorStore, VectorStoreDetails, VectorStoreEntry,
    VectorStoreFileItem, VectorStoreFilesList, VectorStoreList,
};

/// Creates a new vector store.
pub async fn create_vector_store(client: &Client, cfg: &ApiConfig, name: &str) -> Result<String> {
    let url = format!("{BASE_URL}/vector_stores");
    let response = client
        .post(url)
        .bearer_auth(&cfg.api_key)
        .header("OpenAI-Beta", "assistants=v2")
        .json(&VectorStoreCreate {
            name: name.to_string(),
        })
        .send()
        .await
        .with_context(|| "send create_vector_store")?;
    let response = crate::openai_response_for_status(response, "create_vector_store").await?;
    let vector_store: VectorStore = response.json().await?;
    Ok(vector_store.id)
}

/// Lists all vector stores visible to the configured account.
pub async fn list_vector_stores(client: &Client, cfg: &ApiConfig) -> Result<Vec<VectorStoreEntry>> {
    let base_url = format!("{BASE_URL}/vector_stores");
    let mut all_stores = Vec::new();
    let mut after: Option<String> = None;

    loop {
        let mut url = base_url.clone();
        if let Some(cursor) = &after {
            url = format!("{url}?after={cursor}");
        }

        let response = client
            .get(url)
            .bearer_auth(&cfg.api_key)
            .header("OpenAI-Beta", "assistants=v2")
            .send()
            .await
            .with_context(|| "send list_vector_stores")?;
        let response = crate::openai_response_for_status(response, "list_vector_stores").await?;
        let page: VectorStoreList = response.json().await?;
        let has_more = page.has_more;
        after = next_vector_store_cursor(&page);
        all_stores.extend(page.data);

        if !has_more || after.is_none() {
            break;
        }
    }

    Ok(all_stores)
}

/// Fetches aggregate details for a vector store.
pub async fn get_vector_store_details(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
) -> Result<VectorStoreDetails> {
    let url = format!("{BASE_URL}/vector_stores/{vs_id}");
    let response = client
        .get(&url)
        .bearer_auth(&cfg.api_key)
        .header("OpenAI-Beta", "assistants=v2")
        .send()
        .await?;
    let response = crate::openai_response_for_status(response, "get_vector_store_details").await?;
    Ok(response.json().await?)
}

/// Waits for all files in a vector store to reach a ready state.
pub async fn wait_for_vector_store_ready(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
    poll_ms: u64,
    timeout_ms: u64,
) -> Result<()> {
    let start = std::time::Instant::now();

    loop {
        let details = get_vector_store_details(client, cfg, vs_id).await?;
        let counts = &details.file_counts;

        if counts.failed > 0 || counts.cancelled > 0 {
            anyhow::bail!(
                "vector store has {} failed and {} cancelled files",
                counts.failed,
                counts.cancelled
            );
        }

        if counts.in_progress == 0 && counts.total > 0 {
            tracing::debug!("Vector store ready: {} files completed", counts.completed);
            return Ok(());
        }

        if counts.total == 0 {
            tracing::debug!("Vector store is empty, returning early");
            return Ok(());
        }

        if start.elapsed() > Duration::from_millis(timeout_ms) {
            anyhow::bail!(
                "timeout waiting for indexing: {}/{} files still in progress",
                counts.in_progress,
                counts.total
            );
        }

        tracing::debug!(
            "Waiting for indexing: {}/{} in progress",
            counts.in_progress,
            counts.total
        );
        sleep(Duration::from_millis(poll_ms)).await;
    }
}

/// Internal helper that preserves the vector store file relationship response.
pub(crate) async fn add_file_to_vector_store_with_response(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
    file_id: &str,
    attributes: Option<serde_json::Map<String, serde_json::Value>>,
    chunking_strategy: Option<serde_json::Value>,
) -> Result<VectorStoreFileItem> {
    let url = format!("{BASE_URL}/vector_stores/{vs_id}/files");
    let response = client
        .post(url)
        .bearer_auth(&cfg.api_key)
        .header("OpenAI-Beta", "assistants=v2")
        .json(&VectorStoreFileCreate {
            file_id: file_id.to_string(),
            attributes,
            chunking_strategy,
        })
        .send()
        .await?;
    let response =
        crate::openai_response_for_status(response, "add_file_to_vector_store_with_response")
            .await?;
    Ok(response.json().await?)
}

/// Attaches an uploaded file to a vector store.
pub async fn add_file_to_vector_store(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
    file_id: &str,
) -> Result<()> {
    add_file_to_vector_store_with_response(client, cfg, vs_id, file_id, None, None).await?;
    Ok(())
}

/// Attaches an uploaded file with custom attributes and chunking configuration.
pub async fn add_file_to_vector_store_with(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
    file_id: &str,
    attributes: Option<serde_json::Map<String, serde_json::Value>>,
    chunking_strategy: Option<serde_json::Value>,
) -> Result<()> {
    add_file_to_vector_store_with_response(
        client,
        cfg,
        vs_id,
        file_id,
        attributes,
        chunking_strategy,
    )
    .await?;
    Ok(())
}

/// Fetches a specific vector store file relationship.
pub async fn get_vector_store_file(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
    vector_store_file_id: &str,
) -> Result<VectorStoreFileItem> {
    let url = format!("{BASE_URL}/vector_stores/{vs_id}/files/{vector_store_file_id}");
    let response = client
        .get(url)
        .bearer_auth(&cfg.api_key)
        .header("OpenAI-Beta", "assistants=v2")
        .send()
        .await?;
    let response = crate::openai_response_for_status(response, "get_vector_store_file").await?;
    Ok(response.json().await?)
}

/// Waits for a single vector store file relationship to complete indexing.
pub async fn wait_for_vector_store_file_ready(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
    vector_store_file_id: &str,
    poll_ms: u64,
    timeout_ms: u64,
) -> Result<()> {
    let start = std::time::Instant::now();

    loop {
        let file = get_vector_store_file(client, cfg, vs_id, vector_store_file_id).await?;
        match file.status.as_str() {
            "completed" => return Ok(()),
            "failed" | "cancelled" => {
                anyhow::bail!(
                    "vector store file {} is in terminal status '{}'",
                    vector_store_file_id,
                    file.status
                );
            }
            "in_progress" => {}
            status => {
                anyhow::bail!(
                    "vector store file {vector_store_file_id} has unexpected status '{status}'"
                );
            }
        }

        if start.elapsed() > Duration::from_millis(timeout_ms) {
            anyhow::bail!(
                "timeout waiting for vector store file {vector_store_file_id} to finish indexing"
            );
        }

        sleep(Duration::from_millis(poll_ms)).await;
    }
}

/// Legacy polling helper that lists all vector store files every poll interval.
pub async fn wait_for_vector_file_ready(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
    poll_ms: u64,
    timeout_ms: u64,
) -> Result<()> {
    let start = std::time::Instant::now();

    loop {
        let list = list_vector_store_files(client, cfg, vs_id).await?;

        if !list.data.is_empty() {
            if let Some(failed) = list
                .data
                .iter()
                .find(|file| file.status == "failed" || file.status == "cancelled")
            {
                anyhow::bail!(
                    "vector store file {} is in terminal status '{}'",
                    failed.id,
                    failed.status
                );
            }
            if list.data.iter().all(|file| file.status == "completed") {
                break;
            }
        }

        if start.elapsed() > Duration::from_millis(timeout_ms) {
            anyhow::bail!("timeout waiting for indexing");
        }

        sleep(Duration::from_millis(poll_ms)).await;
    }

    Ok(())
}

/// Lists all vector store files across every results page.
pub async fn list_vector_store_files(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
) -> Result<VectorStoreFilesList> {
    let base_url = format!("{BASE_URL}/vector_stores/{vs_id}/files");
    let mut all_files = Vec::new();
    let mut after: Option<String> = None;

    loop {
        let mut url = base_url.clone();
        if let Some(cursor) = &after {
            url = format!("{url}?after={cursor}");
        }

        let response = client
            .get(&url)
            .bearer_auth(&cfg.api_key)
            .header("OpenAI-Beta", "assistants=v2")
            .send()
            .await?;
        let response =
            crate::openai_response_for_status(response, "list_vector_store_files").await?;
        let page: VectorStoreFilesList = response.json().await?;
        let has_more = page.has_more;

        after = next_vector_store_file_cursor(&page);
        all_files.extend(page.data);

        if !has_more || after.is_none() {
            break;
        }
    }

    Ok(VectorStoreFilesList {
        data: all_files,
        has_more: false,
        last_id: None,
    })
}

/// Removes a vector store file relationship without deleting the underlying file.
pub async fn delete_vector_store_file(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
    file_id: &str,
) -> Result<()> {
    let url = format!("{BASE_URL}/vector_stores/{vs_id}/files/{file_id}");
    let response = client
        .delete(url)
        .bearer_auth(&cfg.api_key)
        .header("OpenAI-Beta", "assistants=v2")
        .send()
        .await?;
    let _ = crate::openai_response_for_status(response, "delete_vector_store_file").await?;
    Ok(())
}

/// Fetches metadata for a file in the `OpenAI` Files API.
pub async fn get_file(client: &Client, cfg: &ApiConfig, file_id: &str) -> Result<FileInfo> {
    let url = format!("{BASE_URL}/files/{file_id}");
    let response = client.get(url).bearer_auth(&cfg.api_key).send().await?;
    let response = crate::openai_response_for_status(response, "get_file").await?;
    Ok(response.json().await?)
}

/// Lists vector store files and hydrates each one with full file metadata when available.
pub async fn list_vector_store_files_with_details(
    client: &Client,
    cfg: &ApiConfig,
    vs_id: &str,
) -> Result<Vec<FileInfo>> {
    let files_list = list_vector_store_files(client, cfg, vs_id).await?;
    let mut detailed_files = Vec::new();

    for item in files_list.data {
        let file_id = item
            .file_id
            .as_ref()
            .or_else(|| item.file.as_ref().map(|file| &file.id));

        if let Some(fid) = file_id {
            match get_file(client, cfg, fid).await {
                Ok(file_info) => detailed_files.push(file_info),
                Err(_) => {
                    detailed_files.push(fallback_file_info_from_vector_store_item(&item, fid))
                }
            }
        }
    }

    Ok(detailed_files)
}

fn next_vector_store_cursor(page: &VectorStoreList) -> Option<String> {
    page.last_id
        .clone()
        .or_else(|| page.data.last().map(|store| store.id.clone()))
}

fn next_vector_store_file_cursor(page: &VectorStoreFilesList) -> Option<String> {
    page.last_id
        .clone()
        .or_else(|| page.data.last().map(|file| file.id.clone()))
}

fn fallback_file_info_from_vector_store_item(
    item: &VectorStoreFileItem,
    file_id: &str,
) -> FileInfo {
    FileInfo {
        id: file_id.to_string(),
        filename: item
            .filename
            .clone()
            .or_else(|| item.file.as_ref().and_then(|file| file.filename.clone())),
        purpose: None,
        bytes: None,
        created_at: None,
        attributes: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        fallback_file_info_from_vector_store_item, next_vector_store_cursor,
        next_vector_store_file_cursor,
    };
    use crate::{
        FileInfo, VectorStoreEntry, VectorStoreFileItem, VectorStoreFilesList, VectorStoreList,
    };

    #[test]
    fn vector_store_pagination_prefers_response_last_id() {
        let page = VectorStoreList {
            data: vec![VectorStoreEntry {
                id: "vs_data_id".to_string(),
                name: Some("store".to_string()),
                created_at: None,
            }],
            has_more: true,
            last_id: Some("vs_cursor".to_string()),
        };

        assert_eq!(
            next_vector_store_cursor(&page).as_deref(),
            Some("vs_cursor")
        );
    }

    #[test]
    fn vector_store_file_pagination_prefers_response_last_id() {
        let page = VectorStoreFilesList {
            data: vec![VectorStoreFileItem {
                id: "vsf_data_id".to_string(),
                status: "completed".to_string(),
                file: None,
                file_id: Some("file_1".to_string()),
                filename: None,
                attributes: None,
            }],
            has_more: true,
            last_id: Some("vsf_cursor".to_string()),
        };

        assert_eq!(
            next_vector_store_file_cursor(&page).as_deref(),
            Some("vsf_cursor")
        );
    }

    #[test]
    fn fallback_file_info_preserves_top_level_filename() {
        let item = VectorStoreFileItem {
            id: "vsf_1".to_string(),
            status: "completed".to_string(),
            file: Some(FileInfo {
                id: "file_1".to_string(),
                filename: Some("nested.rs".to_string()),
                purpose: None,
                bytes: None,
                created_at: None,
                attributes: None,
            }),
            file_id: Some("file_1".to_string()),
            filename: Some("top_level.rs".to_string()),
            attributes: None,
        };

        let fallback = fallback_file_info_from_vector_store_item(&item, "file_1");

        assert_eq!(fallback.filename.as_deref(), Some("top_level.rs"));
    }
}
