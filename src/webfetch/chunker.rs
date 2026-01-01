//! Token-aware Markdown chunking for LLM consumption.
//!
//! This module splits Markdown content into chunks that fit within token budgets
//! while preserving logical document structure (headings as boundaries).
//!
//! ## Tokenizer
//!
//! Uses OpenAI's `cl100k_base` tokenizer via the `tiktoken-rs` crate. This is the
//! tokenizer used by GPT-4 and GPT-3.5-turbo, ensuring accurate token counts for
//! those models.
//!
//! ## Chunking Strategy
//!
//! The chunker balances two goals:
//!
//! 1. **Respect token limits**: No chunk exceeds `max_tokens` (default: 600)
//! 2. **Preserve structure**: Prefer splitting at heading boundaries
//!
//! Algorithm:
//! 1. Accumulate lines into the current chunk
//! 2. When a heading (`#`) is encountered, flush the current chunk and start new
//! 3. When token count exceeds limit, flush and start new (mid-section split)
//! 4. Track the most recent heading for context
//!
//! ## Default Token Budget
//!
//! The default of 600 tokens was chosen to:
//! - Keep chunks under typical tool response limits
//! - Preserve enough context for meaningful content
//! - Allow multiple chunks in a single LLM context window

use anyhow::{Context, Result};
use std::sync::OnceLock;
use tiktoken_rs::{CoreBPE, cl100k_base};

/// Default maximum tokens per chunk.
///
/// This value balances context preservation with token budget constraints.
/// 600 tokens is roughly 450-500 words of English text.
const DEFAULT_MAX_TOKENS: usize = 600;

/// Cached `cl100k_base` tokenizer instance (expensive to initialize).
///
/// We store a `Result` so we can surface initialization failures without panicking.
static CL100K_BASE: OnceLock<std::result::Result<CoreBPE, String>> = OnceLock::new();

fn get_encoder() -> Result<&'static CoreBPE> {
    CL100K_BASE
        .get_or_init(|| cl100k_base().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|msg| anyhow::anyhow!("failed to init cl100k_base tokenizer: {}", msg))
}

/// Splits Markdown content into token-budgeted chunks with heading context.
///
/// # Arguments
///
/// * `markdown` - The Markdown text to chunk
/// * `max_tokens` - Optional token limit per chunk (defaults to 600)
///
/// # Returns
///
/// A vector of tuples: `(heading, text, token_count)`
///
/// - `heading`: The most recent Markdown heading before this chunk (`None` if before first heading)
/// - `text`: The chunk's text content
/// - `token_count`: Accurate token count using `cl100k_base`
///
/// # Chunking Behavior
///
/// - Headings (`#`, `##`, etc.) trigger chunk boundaries
/// - Chunks exceeding `max_tokens` are split mid-content
/// - Empty chunks are filtered out
/// - Section boundaries are trimmed; chunk slices preserve exact decoded token boundaries (including
///   any intra-document whitespace at the start/end of a chunk)
///
/// # Example
///
/// ```ignore
/// let chunks = chunk_markdown("# Title\n\nContent here...", Some(500))?;
/// for (heading, text, tokens) in chunks {
///     println!("Section: {:?} ({} tokens)", heading, tokens);
/// }
/// ```
pub fn chunk_markdown(
    markdown: &str,
    max_tokens: Option<usize>,
) -> Result<Vec<(Option<String>, String, usize)>> {
    let encoder = get_encoder()?;
    let max_tokens = max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);

    // We stream through the document and flush at heading boundaries. Each flushed
    // section is then chunked by tokens to avoid O(n^2) re-tokenization on each line.
    let mut chunks: Vec<(Option<String>, String, usize)> = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_text = String::new();

    let mut flush_section = |heading: &Option<String>, text: &str| -> Result<()> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        // Tokenize once for the whole section, then slice tokens into max-sized chunks.
        let tokens = encoder.encode_ordinary(trimmed);
        if tokens.len() <= max_tokens {
            chunks.push((heading.clone(), trimmed.to_string(), tokens.len()));
            return Ok(());
        }

        for slice in tokens.chunks(max_tokens) {
            let decoded = encoder
                .decode(slice.to_vec())
                .context("decode cl100k_base token slice")?;
            // Keep the decoded text as-is so token_count stays exact for the returned text.
            // (Trimming here would require re-tokenizing, which is expensive and can also break
            // indentation-sensitive Markdown like code blocks.)
            if decoded.trim().is_empty() {
                continue;
            }
            chunks.push((heading.clone(), decoded, slice.len()));
        }

        Ok(())
    };

    for line in markdown.lines() {
        let trimmed = line.trim();

        // Headings mark natural chunk boundaries
        if trimmed.starts_with('#') {
            // Flush accumulated content before starting new section
            flush_section(&current_heading, &current_text)?;
            current_text.clear();

            // Extract heading text (strip # prefix)
            current_heading = Some(trimmed.trim_start_matches('#').trim().to_string());
        }

        // Accumulate line into current section
        current_text.push_str(line);
        current_text.push('\n');
    }

    flush_section(&current_heading, &current_text)?;

    Ok(chunks)
}

/// Estimates the token count for a text string.
///
/// Uses the `cl100k_base` tokenizer (GPT-4/GPT-3.5-turbo compatible).
///
/// # Example
///
/// ```ignore
/// let tokens = estimate_tokens("Hello, world!")?;
/// assert_eq!(tokens, 4); // ["Hello", ",", " world", "!"]
/// ```
pub fn estimate_tokens(text: &str) -> Result<usize> {
    let encoder = get_encoder()?;
    Ok(encoder.encode_ordinary(text).len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_nonzero_for_simple_text() {
        let n = estimate_tokens("Hello, world!").expect("token estimate failed");
        assert!(n > 0);
    }

    #[test]
    fn chunk_markdown_respects_heading_boundaries() {
        let md = r#"
Intro line

# Title
Body line 1

## Sub
Body line 2
"#;

        let chunks = chunk_markdown(md, Some(10_000)).expect("chunking failed");
        assert!(
            chunks.len() >= 3,
            "expected at least 3 chunks (intro, Title section, Sub section), got {}",
            chunks.len()
        );

        let (h0, t0, n0) = &chunks[0];
        assert!(h0.is_none());
        assert!(t0.contains("Intro line"));
        assert!(*n0 > 0);

        let (h1, t1, n1) = &chunks[1];
        assert_eq!(h1.as_deref(), Some("Title"));
        assert!(t1.contains("# Title"));
        assert!(t1.contains("Body line 1"));
        assert!(*n1 > 0);

        let (h2, t2, n2) = &chunks[2];
        assert_eq!(h2.as_deref(), Some("Sub"));
        assert!(t2.contains("## Sub"));
        assert!(t2.contains("Body line 2"));
        assert!(*n2 > 0);
    }

    #[test]
    fn chunk_markdown_never_exceeds_max_tokens_when_splitting() {
        let md = "hello world ".repeat(10_000);
        let max_tokens = 32;
        let chunks = chunk_markdown(&md, Some(max_tokens)).expect("chunking failed");
        assert!(!chunks.is_empty());
        for (_heading, _text, token_count) in chunks {
            assert!(
                token_count <= max_tokens,
                "chunk token_count {token_count} exceeded max_tokens {max_tokens}"
            );
        }
    }
}
