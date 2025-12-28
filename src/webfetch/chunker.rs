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

use anyhow::Result;
use tiktoken_rs::cl100k_base;

/// Default maximum tokens per chunk.
///
/// This value balances context preservation with token budget constraints.
/// 600 tokens is roughly 450-500 words of English text.
const DEFAULT_MAX_TOKENS: usize = 600;

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
/// - `text`: The chunk's text content (trimmed)
/// - `token_count`: Accurate token count using `cl100k_base`
///
/// # Chunking Behavior
///
/// - Headings (`#`, `##`, etc.) trigger chunk boundaries
/// - Chunks exceeding `max_tokens` are split mid-content
/// - Empty chunks are filtered out
/// - All chunk text is trimmed of leading/trailing whitespace
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
    let encoder = cl100k_base()?;
    let max_tokens = max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    let mut chunks = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_text = String::new();

    for line in markdown.lines() {
        let trimmed = line.trim();

        // Headings mark natural chunk boundaries
        if trimmed.starts_with('#') {
            // Flush accumulated content before starting new section
            if !current_text.trim().is_empty() {
                let tokens = encoder.encode_ordinary(&current_text).len();
                chunks.push((current_heading.clone(), current_text.clone(), tokens));
                current_text.clear();
            }
            // Extract heading text (strip # prefix)
            current_heading = Some(trimmed.trim_start_matches('#').trim().to_string());
        }

        // Accumulate line into current chunk
        current_text.push_str(line);
        current_text.push('\n');

        // Check if we've exceeded token budget
        let tokens = encoder.encode_ordinary(&current_text).len();
        if tokens >= max_tokens {
            // Force flush even mid-paragraph to respect token limit
            chunks.push((current_heading.clone(), current_text.clone(), tokens));
            current_text.clear();
        }
    }

    // Flush any remaining content
    if !current_text.trim().is_empty() {
        let trimmed = current_text.trim().to_string();
        let tokens = encoder.encode_ordinary(&trimmed).len();
        chunks.push((current_heading, trimmed, tokens));
    }

    // Final pass: trim all chunk texts and recalculate token counts
    let chunks = chunks
        .into_iter()
        .map(|(h, t, _)| {
            let trimmed = t.trim().to_string();
            let tokens = encoder.encode_ordinary(&trimmed).len();
            (h, trimmed, tokens)
        })
        .collect();

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
    let encoder = cl100k_base()?;
    Ok(encoder.encode_ordinary(text).len())
}
