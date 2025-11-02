use anyhow::Result;
use tiktoken_rs::cl100k_base;

// Default chunk budget keeps outputs under typical Codex tool-call limits while preserving context.
const DEFAULT_MAX_TOKENS: usize = 600;

/// Split markdown content into logical sections with token counts.
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
        if trimmed.starts_with('#') {
            // flush current chunk if non-empty
            if !current_text.trim().is_empty() {
                let tokens = encoder.encode_ordinary(&current_text).len();
                chunks.push((current_heading.clone(), current_text.clone(), tokens));
                current_text.clear();
            }
            current_heading = Some(trimmed.trim_start_matches('#').trim().to_string());
        }
        current_text.push_str(line);
        current_text.push('\n');
        let tokens = encoder.encode_ordinary(&current_text).len();
        if tokens >= max_tokens {
            chunks.push((current_heading.clone(), current_text.clone(), tokens));
            current_text.clear();
        }
    }

    if !current_text.trim().is_empty() {
        let tokens = encoder.encode_ordinary(&current_text).len();
        chunks.push((current_heading, current_text, tokens));
    }

    Ok(chunks)
}

pub fn estimate_tokens(text: &str) -> Result<usize> {
    let encoder = cl100k_base()?;
    Ok(encoder.encode_ordinary(text).len())
}
