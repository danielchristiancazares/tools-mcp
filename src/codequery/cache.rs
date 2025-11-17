use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tracing::warn;

// CodeQuery keeps a tiny cache of vector store IDs on disk so repeated lookups avoid an API call.
// We isolate this logic so the handler can focus on request validation and orchestration.

pub fn load_store_id_from_cache(name: &str) -> Option<String> {
    let cache = load_store_cache();
    cache.get(name).cloned()
}

pub fn cache_store_id(name: &str, id: &str) {
    let mut cache = load_store_cache();
    if cache
        .get(name)
        .map(|existing| existing == id)
        .unwrap_or(false)
    {
        return;
    }
    cache.insert(name.to_string(), id.to_string());
    if let Err(err) = write_store_cache(&cache) {
        warn!("Failed to persist CodeQuery store cache: {}", err);
    }
}

fn load_store_cache() -> HashMap<String, String> {
    let Some(path) = stores_cache_path() else {
        return HashMap::new();
    };

    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    "Failed to read CodeQuery store cache at {}: {}",
                    path.display(),
                    err
                );
            }
            return HashMap::new();
        }
    };

    match serde_json::from_str::<HashMap<String, String>>(&contents) {
        Ok(cache) => cache,
        Err(err) => {
            warn!(
                "Ignoring invalid CodeQuery store cache at {}: {}",
                path.display(),
                err
            );
            HashMap::new()
        }
    }
}

fn write_store_cache(cache: &HashMap<String, String>) -> Result<()> {
    let Some(path) = stores_cache_path() else {
        warn!("Skipping CodeQuery store cache write because HOME is unset");
        return Ok(());
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create CodeQuery store cache directory {}",
                parent.display()
            )
        })?;
    }

    let payload =
        serde_json::to_string_pretty(cache).context("failed to serialize CodeQuery store cache")?;
    fs::write(&path, payload).with_context(|| {
        format!(
            "failed to write CodeQuery store cache at {}",
            path.display()
        )
    })?;
    Ok(())
}

fn stores_cache_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        let mut path = PathBuf::from(home);
        path.push(".codex");
        path.push("mcp");
        path.push("stores.json");
        path
    })
}
