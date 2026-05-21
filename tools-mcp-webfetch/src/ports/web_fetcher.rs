//! Outbound port: fetch remote documents for `WebFetch`.

use anyhow::Result;
use std::pin::Pin;

/// Fetches a URL and returns the structured `WebFetch` response (HTTP and/or browser pipeline).
pub(crate) trait WebFetcher: Send + Sync {
    fn fetch<'a>(
        &'a self,
        req: crate::webfetch::FetchRequest,
    ) -> Pin<
        Box<dyn std::future::Future<Output = Result<crate::webfetch::FetchResponse>> + Send + 'a>,
    >;
}
