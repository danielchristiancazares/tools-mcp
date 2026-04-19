//! Process-wide default service instances (composition-owned adapters).
//!
//! Keeps tool handlers free of global singletons while preserving the previous behavior of
//! using the default `WebFetch` pipeline everywhere.

use std::sync::{Arc, OnceLock};

use crate::adapters::outbound::RunFetchWebFetcher;
use crate::ports::WebFetcher;

static DEFAULT_WEB_FETCHER: OnceLock<Arc<dyn WebFetcher>> = OnceLock::new();

/// Shared default `WebFetch` adapter (wraps [`crate::webfetch::run_fetch`]).
pub(crate) fn default_web_fetcher() -> Arc<dyn WebFetcher> {
    DEFAULT_WEB_FETCHER
        .get_or_init(|| {
            let r: Arc<dyn WebFetcher> = Arc::new(RunFetchWebFetcher);
            r
        })
        .clone()
}
