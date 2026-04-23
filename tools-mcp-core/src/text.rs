//! Text helpers shared across tool crates.

/// Strips ANSI escape codes from a string, returning clean plaintext.
///
/// Handles CSI sequences (`ESC [ ... final_byte`), OSC sequences
/// (`ESC ] ... BEL` or `ESC ] ... ESC \`), character-set designation
/// (`ESC ( G` / `ESC ) G`), and single-character escapes (e.g. `ESC M`).
///
/// # Examples
///
/// ```
/// use tools_mcp_core::text::strip_ansi_codes;
///
/// let colored = "\x1b[31mError:\x1b[0m file not found";
/// assert_eq!(strip_ansi_codes(colored), "Error: file not found");
/// ```
pub fn strip_ansi_codes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '\x1b' {
            result.push(c);
            continue;
        }

        match chars.peek() {
            Some('[') => {
                // CSI: ESC [ <params> <final_byte in 0x40..=0x7E>
                chars.next();
                while let Some(&ch) = chars.peek() {
                    chars.next();
                    if ('@'..='~').contains(&ch) {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC: ESC ] <data> (BEL | ESC \)
                chars.next();
                while let Some(&ch) = chars.peek() {
                    if ch == '\x07' {
                        chars.next();
                        break;
                    }
                    if ch == '\x1b' {
                        chars.next();
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                        continue;
                    }
                    chars.next();
                }
            }
            Some('(' | ')') => {
                // Character-set designation: ESC ( G  or  ESC ) G
                chars.next();
                chars.next();
            }
            _ => {
                // Other single-char escapes (e.g. ESC M reverse linefeed)
                chars.next();
            }
        }
    }

    result
}

/// Truncates `s` to at most `max_chars` Unicode scalar values at a char boundary,
/// appending `…` when truncation actually occurs.
///
/// Iterates byte indices rather than collecting the chars so short inputs avoid
/// any allocation and long inputs avoid collecting a full char vector.
pub fn truncate_at_char_boundary(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return if s.is_empty() { String::new() } else { "…".to_string() };
    }

    let mut truncation_byte_idx = s.len();
    for (char_count, (byte_idx, _)) in s.char_indices().enumerate() {
        if char_count == max_chars {
            truncation_byte_idx = byte_idx;
            break;
        }
    }

    if truncation_byte_idx < s.len() {
        format!("{}…", &s[..truncation_byte_idx])
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{strip_ansi_codes, truncate_at_char_boundary};

    #[test]
    fn strip_ansi_codes_nested_osc_csi() {
        // OSC sequence containing an ESC but not an ST (ESC \), followed by BEL.
        // Old buggy behavior would stop at the first ESC and leave "[31mcolor" in output.
        let input = "\x1b]test\x1b[31mcolor\x07actual_content";
        assert_eq!(strip_ansi_codes(input), "actual_content");
    }

    #[test]
    fn strip_ansi_codes_colors() {
        assert_eq!(
            strip_ansi_codes("\x1b[1;31;40mBold red on black\x1b[0m"),
            "Bold red on black"
        );
    }

    #[test]
    fn strip_ansi_codes_passthrough() {
        assert_eq!(strip_ansi_codes("Hello, world!"), "Hello, world!");
    }

    #[test]
    fn returns_unchanged_when_short() {
        assert_eq!(truncate_at_char_boundary("abc", 10), "abc");
    }

    #[test]
    fn truncates_ascii_with_ellipsis() {
        assert_eq!(truncate_at_char_boundary("abcdef", 3), "abc…");
    }

    #[test]
    fn truncates_on_multibyte_boundary() {
        assert_eq!(truncate_at_char_boundary("éééé", 3), "ééé…");
    }

    #[test]
    fn handles_multi_scalar_emoji() {
        let input = "🎉🎊🎈🎁🎀";
        assert_eq!(truncate_at_char_boundary(input, 3), "🎉🎊🎈…");
    }

    #[test]
    fn zero_max_chars_on_nonempty_input_returns_ellipsis() {
        assert_eq!(truncate_at_char_boundary("abc", 0), "…");
    }

    #[test]
    fn zero_max_chars_on_empty_input_returns_empty() {
        assert_eq!(truncate_at_char_boundary("", 0), "");
    }

    #[test]
    fn truncates_across_3_byte_boundary_without_panic() {
        // Exercises the original bug: € is 3 bytes, spans the naive slice point.
        let prefix = "a".repeat(1198);
        let input = format!("{prefix}€tail");
        assert!(input.len() > 1200);

        let result = truncate_at_char_boundary(&input, 1200);
        assert!(result.ends_with('…'));
        assert!(result.contains('€'));
        assert_eq!(result.chars().count(), 1201); // 1200 + ellipsis
    }
}
