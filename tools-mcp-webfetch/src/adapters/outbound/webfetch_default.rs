//! Default `WebFetch` adapter: delegates to [`crate::webfetch::run_fetch`] unchanged.

use crate::ports::WebFetcher;
use anyhow::Result;
use std::pin::Pin;

/// Runs the existing `WebFetch` pipeline (SSRF, robots, cache, browser fallback, chunking).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct RunFetchWebFetcher;

impl WebFetcher for RunFetchWebFetcher {
    fn fetch<'a>(
        &'a self,
        req: crate::webfetch::FetchRequest,
    ) -> Pin<
        Box<dyn std::future::Future<Output = Result<crate::webfetch::FetchResponse>> + Send + 'a>,
    > {
        Box::pin(crate::webfetch::run_fetch(req))
    }
}
