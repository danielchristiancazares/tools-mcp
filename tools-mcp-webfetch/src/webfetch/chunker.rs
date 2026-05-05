//! Token-aware Markdown chunking for LLM consumption.
//!
//! This module splits Markdown content into chunks that fit within token budgets
//! while preserving logical document structure (headings as boundaries).
//!
//! ## Tokenizer
//!
//! Uses `OpenAI`'s `cl100k_base` tokenizer via the `tiktoken-rs` crate. This is the
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
use tiktoken_rs::{CoreBPE, Rank, cl100k_base};

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
        .map_err(|msg| anyhow::anyhow!("failed to init cl100k_base tokenizer: {msg}"))
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
    anyhow::ensure!(max_tokens > 0, "max_chunk_tokens must be greater than 0");

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

        // IMPORTANT: `tiktoken-rs` token byte sequences are not guaranteed to align with UTF-8
        // character boundaries. If we slice the token array at arbitrary indices and decode each
        // slice independently, we can cut multi-byte UTF-8 sequences in half, producing invalid
        // UTF-8 and failing with errors like:
        // "Unable to decode into a valid UTF-8 string: incomplete".
        //
        // To make chunking robust, we enforce `max_tokens` while also ensuring each chunk ends on
        // a token boundary that is also a UTF-8 boundary.
        let mut start = 0usize;
        while start < tokens.len() {
            let window_end = (start + max_tokens).min(tokens.len());
            let (decoded, used_tokens) =
                decode_utf8_safe_token_prefix(encoder, &tokens[start..window_end])
                    .context("decode cl100k_base token slice (utf8-safe)")?;

            // Always make progress, even if the decoded slice is whitespace-only.
            start += used_tokens;

            // Keep the decoded text as-is so token_count stays exact for the returned text.
            // (Trimming here would require re-tokenizing, which is expensive and can also break
            // indentation-sensitive Markdown like code blocks.)
            if decoded.trim().is_empty() {
                continue;
            }
            chunks.push((heading.clone(), decoded, used_tokens));
        }

        Ok(())
    };

    // Track fenced code blocks so we don't treat headings inside code as section boundaries.
    // Markdown allows variable-length fences (>=3) using either backticks or tildes, and the
    // closing fence must use the same marker with at least the opening length.
    let mut code_fence: Option<(char, usize)> = None;

    for line in markdown.lines() {
        let trimmed = line.trim();

        // Track fenced code blocks (` ``` ` / ` ~~~ `), including variable-length fences.
        if let Some((marker, len)) = parse_fence_marker(trimmed) {
            match code_fence {
                None => {
                    code_fence = Some((marker, len));
                }
                Some((open_marker, open_len))
                    if marker == open_marker
                        && len >= open_len
                        && is_closing_fence_line(trimmed) =>
                {
                    code_fence = None;
                }
                _ => {}
            }
        }

        // Headings mark natural chunk boundaries, but only outside code blocks.
        let heading_text = if code_fence.is_none() {
            parse_markdown_heading(trimmed)
        } else {
            None
        };
        if let Some(heading_text) = heading_text {
            // Flush accumulated content before starting new section
            flush_section(&current_heading, &current_text)?;
            current_text.clear();

            current_heading = Some(heading_text);
        }

        // Accumulate line into current section
        current_text.push_str(line);
        current_text.push('\n');
    }

    flush_section(&current_heading, &current_text)?;

    Ok(chunks)
}

fn parse_fence_marker(trimmed_line: &str) -> Option<(char, usize)> {
    let mut chars = trimmed_line.chars();
    let marker = chars.next()?;
    if marker != '`' && marker != '~' {
        return None;
    }

    let len = trimmed_line.chars().take_while(|&c| c == marker).count();
    (len >= 3).then_some((marker, len))
}

fn is_closing_fence_line(trimmed_line: &str) -> bool {
    // Closing fence line may contain surrounding whitespace but no info string/content.
    // We call this only after confirming marker + minimum length.
    let mut chars = trimmed_line.chars();
    let Some(marker) = chars.next() else {
        return false;
    };
    let rest = trimmed_line
        .chars()
        .skip_while(|&c| c == marker)
        .collect::<String>();
    rest.trim().is_empty()
}

fn parse_markdown_heading(trimmed_line: &str) -> Option<String> {
    let bytes = trimmed_line.as_bytes();
    let hashes = bytes.iter().take_while(|&&b| b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }

    if hashes < bytes.len() && !bytes[hashes].is_ascii_whitespace() {
        return None;
    }

    let mut text = trimmed_line[hashes..].trim();
    let without_trailing = text.trim_end();
    if without_trailing.ends_with('#') {
        let hash_start = without_trailing.trim_end_matches('#').len();
        let before_hashes = &without_trailing[..hash_start];
        if before_hashes
            .chars()
            .last()
            .is_some_and(char::is_whitespace)
        {
            text = before_hashes.trim_end();
        }
    }

    Some(text.to_string())
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

/// Decodes a token slice into a valid UTF-8 `String`.
///
/// `CoreBPE::decode(Vec<Rank>)` validates UTF-8 and can fail if the decoded bytes start/end in the
/// middle of a multi-byte UTF-8 sequence. This can happen when we chunk by tokens: token byte
/// sequences are not guaranteed to align with UTF-8 character boundaries.
///
/// This function decodes the provided token slice to raw bytes first and returns the largest
/// prefix (measured in whole tokens) that forms valid UTF-8.
fn decode_utf8_safe_token_prefix(encoder: &CoreBPE, tokens: &[Rank]) -> Result<(String, usize)> {
    anyhow::ensure!(!tokens.is_empty(), "empty token slice");

    // Fast path: most token windows decode cleanly. Avoid any extra bookkeeping unless we hit a
    // UTF-8 validation failure.
    if let Ok(s) = encoder.decode(tokens.to_vec()) {
        return Ok((s, tokens.len()));
    }

    // Decode to raw bytes (no UTF-8 validation) and record byte-length boundaries after each token.
    let mut bytes: Vec<u8> = Vec::new();
    let mut byte_ends: Vec<usize> = Vec::with_capacity(tokens.len());
    for token_bytes in encoder._decode_native_and_split(tokens.to_vec()) {
        bytes.extend_from_slice(&token_bytes);
        byte_ends.push(bytes.len());
    }

    let utf8_err = match std::str::from_utf8(&bytes) {
        Ok(s) => return Ok((s.to_string(), tokens.len())),
        Err(e) => e,
    };

    let valid_up_to = utf8_err.valid_up_to();

    // Find the largest token boundary whose byte offset is <= valid_up_to.
    let keep_tokens = match byte_ends.binary_search(&valid_up_to) {
        Ok(idx) => idx + 1,
        Err(pos) => pos,
    };

    // This should be unreachable if we only start chunks at UTF-8 boundaries, but be defensive:
    // fall back to a lossy decode of a single token so we always make progress.
    if keep_tokens == 0 {
        let first = encoder
            ._decode_native_and_split(vec![tokens[0]])
            .next()
            .unwrap_or_default();
        return Ok((String::from_utf8_lossy(&first).to_string(), 1));
    }

    let byte_end = byte_ends[keep_tokens - 1];
    let s = std::str::from_utf8(&bytes[..byte_end]).context("utf8 prefix")?;
    Ok((s.to_string(), keep_tokens))
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
        let md = r"
Intro line

# Title
Body line 1

## Sub
Body line 2
";

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

    #[test]
    fn chunk_markdown_rejects_zero_max_tokens() {
        let err = chunk_markdown("hello", Some(0))
            .expect_err("zero token budget should be rejected explicitly");
        assert!(
            err.to_string().contains("greater than 0"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn chunk_markdown_does_not_split_inside_code_blocks() {
        // BUG: The chunker treats `# comment` inside fenced code blocks as headings
        let md = "# Title\n```python\n# This is a comment\nx = 1\n```";
        let chunks = chunk_markdown(md, None).unwrap();

        // If the bug exists, it splits at "# This is a comment" producing 2 chunks.
        // Correct behavior: 1 chunk containing the full code block.
        assert_eq!(
            chunks.len(),
            1,
            "Should not split inside a fenced code block. Got {} chunks: {:?}",
            chunks.len(),
            chunks.iter().map(|(h, t, _)| (h, t)).collect::<Vec<_>>()
        );
        assert!(
            chunks[0].1.contains("```python\n# This is a comment"),
            "Chunk should contain the full code block"
        );
    }

    #[test]
    fn chunk_markdown_does_not_treat_hash_prefixed_words_as_headings() {
        let md = "# Title\nparagraph\n#hashtag is text, not a heading\nstill title\n## Sub\nbody";
        let chunks = chunk_markdown(md, None).unwrap();

        assert_eq!(chunks.len(), 2, "unexpected chunks: {chunks:?}");
        assert_eq!(chunks[0].0.as_deref(), Some("Title"));
        assert!(chunks[0].1.contains("#hashtag is text"));
        assert_eq!(chunks[1].0.as_deref(), Some("Sub"));
    }

    #[test]
    fn chunk_markdown_ignores_invalid_seven_hash_heading() {
        let md = "# Title\n####### not a commonmark heading\nstill title";
        let chunks = chunk_markdown(md, None).unwrap();

        assert_eq!(chunks.len(), 1, "unexpected chunks: {chunks:?}");
        assert_eq!(chunks[0].0.as_deref(), Some("Title"));
        assert!(chunks[0].1.contains("####### not a commonmark heading"));
    }

    #[test]
    fn chunk_markdown_respects_variable_length_fences() {
        let md =
            "# Title\n````markdown\n# Not heading\n```\nstill code\n````\n## Real heading\nbody";
        let chunks = chunk_markdown(md, None).unwrap();

        assert_eq!(
            chunks.len(),
            2,
            "Variable-length fences should keep inner content in the same section"
        );
        assert_eq!(chunks[0].0.as_deref(), Some("Title"));
        assert!(chunks[0].1.contains("# Not heading"));
        assert!(chunks[0].1.contains("still code"));
        assert_eq!(chunks[1].0.as_deref(), Some("Real heading"));
    }

    #[test]
    fn chunk_markdown_respects_tilde_fences() {
        let md = "# Title\n~~~python\n# Not heading\n~~~\n## Real heading\nbody";
        let chunks = chunk_markdown(md, None).unwrap();

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].0.as_deref(), Some("Title"));
        assert!(chunks[0].1.contains("# Not heading"));
        assert_eq!(chunks[1].0.as_deref(), Some("Real heading"));
    }

    #[test]
    fn chunk_markdown_is_utf8_safe_for_token_boundaries() {
        // Regression test for the historical bug where naive token chunking
        // (`tokens.chunks(max_tokens)`) can split multi-byte UTF-8 sequences.
        //
        // Rather than hard-coding a single problematic character (tokenization can differ across
        // vocab updates), we search a small, fixed set of "PDF-ish" Unicode characters/patterns
        // until we find one that fails under naive token slicing.
        let encoder = get_encoder().expect("tokenizer init failed");
        let max_tokens = 64;

        let candidates: &[char] = &[
            '\u{00A0}', // NO-BREAK SPACE (common in PDF text)
            '\u{202F}', // NARROW NO-BREAK SPACE
            '\u{200B}', // ZERO WIDTH SPACE
            '\u{2060}', // WORD JOINER
            '\u{FEFF}', // ZERO WIDTH NO-BREAK SPACE / BOM
            '€', '—', '…', '漢', '😀',
        ];

        let mut found: Option<String> = None;
        'outer: for &ch in candidates {
            for reps in [128usize, 256, 512] {
                let md = format!("A{ch}").repeat(reps);
                let tokens = encoder.encode_ordinary(&md);
                if tokens.len() <= max_tokens {
                    continue;
                }

                // Simulate the buggy behavior: fixed token chunking + UTF-8 validating decode.
                let naive_fails = tokens
                    .chunks(max_tokens)
                    .any(|slice| encoder.decode(slice.to_vec()).is_err());
                if naive_fails {
                    found = Some(md);
                    break 'outer;
                }
            }
        }

        let md = found.expect(
            "failed to find a deterministic repro for naive token slicing; update candidates",
        );

        let chunks = chunk_markdown(&md, Some(max_tokens)).expect("chunking failed");
        assert!(!chunks.is_empty());
        for (_heading, text, token_count) in chunks {
            assert!(
                token_count <= max_tokens,
                "chunk token_count {token_count} exceeded max_tokens {max_tokens}"
            );
            assert!(std::str::from_utf8(text.as_bytes()).is_ok());
        }
    }

    #[tokio::test]
    #[ignore = "requires network access"]
    async fn test_real_website_chunking() {
        // Fetch a real website with Python code examples
        // Try multiple sites with code examples
        let urls = [
            "https://docs.python.org/3/tutorial/inputoutput.html",
            "https://realpython.com/python-comments-guide/",
            "https://www.digitalocean.com/community/tutorials/how-to-write-comments-in-python-3",
        ];

        for url in urls {
            eprintln!("\n\n========== TESTING: {url} ==========");
            test_url(url).await;
        }
    }

    async fn test_url(url: &str) {
        let request = super::super::types::FetchRequest {
            url: url.to_string(),
            max_chunk_tokens: Some(600),
            no_cache: true,
            force_browser: false,
        };

        let response = super::super::run_fetch(request)
            .await
            .expect("fetch failed");

        eprintln!("\n=== FETCHED: {url} ===");
        eprintln!(
            "Chunks: {}, Method: {}",
            response.chunks.len(),
            response.rendering_method
        );

        // Find chunks with unbalanced code fences
        let mut broken = Vec::new();
        for (i, chunk) in response.chunks.iter().enumerate() {
            let count = chunk.text.matches("```").count();
            if count % 2 != 0 {
                broken.push((i, count, chunk.heading.clone(), chunk.text.clone()));
            }
        }

        if broken.is_empty() {
            eprintln!("\nNo broken chunks found.");
            // Show some chunks anyway
            for (i, chunk) in response.chunks.iter().take(5).enumerate() {
                eprintln!(
                    "\n--- Chunk {} ({:?}) ---\n{}",
                    i,
                    chunk.heading,
                    &chunk.text[..chunk.text.len().min(200)]
                );
            }
        } else {
            eprintln!("\n=== BROKEN CHUNKS ({}) ===", broken.len());
            for (i, count, heading, text) in &broken {
                eprintln!("\n--- Chunk {i} (heading: {heading:?}, {count} backticks) ---");
                eprintln!("{}", &text[..text.len().min(300)]);
            }
        }
    }
}
