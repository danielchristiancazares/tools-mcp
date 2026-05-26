mod adapters;
mod ports;
mod services;
mod tools;
mod webfetch_tool;

mod webfetch;

use tools_mcp_core::ToolRegistry;

pub fn register_tools(registry: &mut ToolRegistry) {
    registry.register::<tools::WebFetchTool>();
}

#[doc(hidden)]
pub fn benchmark_browser_available() -> bool {
    webfetch::browser::BrowserPool::is_available()
}

#[doc(hidden)]
pub fn benchmark_chunk_markdown(markdown: &str, max_tokens: usize) -> usize {
    webfetch::chunker::chunk_markdown(markdown, Some(max_tokens))
        .expect("benchmark markdown chunking should succeed")
        .into_iter()
        .map(|(_, _, token_count)| token_count)
        .sum()
}

#[doc(hidden)]
pub fn benchmark_extract_text_len(bytes: &[u8]) -> usize {
    webfetch::extract::extract(bytes, Some("text/plain"), "benchmark://text")
        .expect("benchmark text extraction should succeed")
        .markdown
        .len()
}

#[doc(hidden)]
pub fn benchmark_clean_markdown_len(html: &str) -> usize {
    webfetch::extract::benchmark_clean_markdown_len(html)
}
