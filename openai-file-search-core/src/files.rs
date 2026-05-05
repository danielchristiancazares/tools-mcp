use anyhow::{Context, Result};
use reqwest::{Client, multipart};
use tokio::time::{Duration, sleep};

use crate::vector_stores::{
    add_file_to_vector_store_with_response, wait_for_vector_store_file_ready,
};
use crate::{ApiConfig, BASE_URL, FileObj, compute_upload_filename};

/// Uploads a local file or URL to the `OpenAI` Files API.
pub async fn upload_file(client: &Client, cfg: &ApiConfig, path_or_url: &str) -> Result<String> {
    let url = format!("{BASE_URL}/files");
    let form = if path_or_url.starts_with("http://") || path_or_url.starts_with("https://") {
        let bytes = client
            .get(path_or_url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let name = upload_name_from_url(path_or_url);
        let effective_name = compute_upload_filename(name);
        let part = multipart::Part::bytes(bytes.to_vec()).file_name(effective_name.into_owned());
        multipart::Form::new()
            .part("file", part)
            .text("purpose", "assistants")
    } else {
        let bytes = tokio::fs::read(path_or_url)
            .await
            .with_context(|| format!("opening {path_or_url}"))?;
        let name = std::path::Path::new(path_or_url)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
            .to_string();
        let effective_name = compute_upload_filename(&name);
        let part = multipart::Part::bytes(bytes).file_name(effective_name.into_owned());
        multipart::Form::new()
            .part("file", part)
            .text("purpose", "assistants")
    };

    let response = client
        .post(url)
        .bearer_auth(&cfg.api_key)
        .multipart(form)
        .send()
        .await?;
    let response = crate::openai_response_for_status(response, "upload_file").await?;
    let uploaded: FileObj = response.json().await?;
    Ok(uploaded.id)
}

fn upload_name_from_url(path_or_url: &str) -> &str {
    let without_query = path_or_url.split(['?', '#']).next().unwrap_or(path_or_url);
    let path = if let Some((_, rest)) = without_query.split_once("://") {
        rest.split_once('/').map(|(_, path)| path).unwrap_or("")
    } else {
        without_query
    };

    path.rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or("file")
}

/// Uploads multiple files and attaches them to a vector store.
pub async fn upload_files_batch(
    client: &Client,
    cfg: &ApiConfig,
    file_paths: Vec<String>,
    vector_store_id: &str,
    concurrent_limit: usize,
) -> Result<(Vec<(String, String)>, Vec<(String, String)>)> {
    use futures::stream::{self, StreamExt};

    let mut successes = Vec::new();
    let mut failures = Vec::new();

    let chunk_count = if file_paths.is_empty() {
        0
    } else {
        file_paths.len().div_ceil(concurrent_limit)
    };

    for (chunk_idx, chunk) in file_paths.chunks(concurrent_limit).enumerate() {
        tracing::info!("Processing chunk {}/{}", chunk_idx + 1, chunk_count);

        let results: Vec<_> = stream::iter(chunk.iter().cloned())
            .map(|path| async move {
                let file_id = match upload_file(client, cfg, &path).await {
                    Ok(id) => id,
                    Err(err) => {
                        tracing::error!("Failed to upload {}: {}", path, err);
                        return Err((path, format!("Upload failed: {err}")));
                    }
                };

                match add_file_to_vector_store_with_response(
                    client,
                    cfg,
                    vector_store_id,
                    &file_id,
                    None,
                    None,
                )
                .await
                {
                    Ok(vector_store_file) => {
                        if let Err(err) = wait_for_vector_store_file_ready(
                            client,
                            cfg,
                            vector_store_id,
                            &vector_store_file.id,
                            1000,
                            30_000,
                        )
                        .await
                        {
                            tracing::warn!(
                                "File {} uploaded but processing incomplete: {}",
                                path,
                                err
                            );
                        }
                        tracing::info!("Successfully uploaded and attached: {}", path);
                        Ok((path, file_id))
                    }
                    Err(err) => {
                        tracing::error!("Failed to attach {} to store: {}", path, err);
                        Err((path, format!("Attach failed: {err}")))
                    }
                }
            })
            .buffer_unordered(concurrent_limit)
            .collect()
            .await;

        for result in results {
            match result {
                Ok((path, file_id)) => successes.push((path, file_id)),
                Err((path, error)) => failures.push((path, error)),
            }
        }

        if chunk_idx + 1 < chunk_count {
            sleep(Duration::from_millis(1000)).await;
        }
    }

    Ok((successes, failures))
}

#[cfg(test)]
mod tests {
    use super::upload_name_from_url;

    #[test]
    fn upload_name_from_url_strips_query_and_fragment() {
        assert_eq!(
            upload_name_from_url("https://example.com/src/report.pdf?v=2#page=1"),
            "report.pdf"
        );
    }

    #[test]
    fn upload_name_from_url_defaults_when_path_has_no_filename() {
        assert_eq!(
            upload_name_from_url("https://example.com?download=1"),
            "file"
        );
        assert_eq!(upload_name_from_url("https://example.com/"), "file");
    }
}
