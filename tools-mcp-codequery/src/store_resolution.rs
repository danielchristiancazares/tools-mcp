//! Vector-store name → ID resolution with a tiered lookup strategy.
//!
//! Resolution order:
//! 1. Local cache (`~/.codex/mcp/stores.json`) keyed by workspace fingerprint or store name.
//! 2. OpenAI list-stores API, matching by name.
//! 3. Auto-create a new store if no match exists.
//!
//! Each successful API or creation result is written back to the cache so subsequent
//! lookups are instant across process restarts.

use anyhow::Result;
use reqwest::Client;

use crate::codequery_cache::{cache_store_id, load_store_id_from_cache};

pub(crate) async fn resolve_vector_store_id(
    client: &Client,
    cfg: &openai_file_search_core::ApiConfig,
    cache_lookup_key: &str,
    remote_name: &str,
) -> Result<String> {
    if let Some(id) = load_store_id_from_cache(cache_lookup_key) {
        return Ok(id);
    }

    // Fall back to the API when the cache misses so the happy-path stays fast after the
    // first lookup without requiring manual list-stores calls.
    let stores = openai_file_search_core::list_vector_stores(client, cfg).await?;
    if let Some(entry) = stores
        .into_iter()
        .find(|entry| entry.name.as_deref() == Some(remote_name))
    {
        cache_store_id(cache_lookup_key, &entry.id);
        return Ok(entry.id);
    }

    // Absent a matching store we create one automatically so new clones come online without
    // manual setup. This favors seamless agent startup over requiring explicit provisioning.
    let new_id = openai_file_search_core::create_vector_store(client, cfg, remote_name).await?;
    cache_store_id(cache_lookup_key, &new_id);
    Ok(new_id)
}
