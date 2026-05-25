//! Text helpers shared across tool crates.

use memchr::memchr;

const ESC: u8 = b'\x1b';

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
    let bytes = s.as_bytes();
    let Some(mut marker) = find_next_esc(bytes, 0) else {
        return s.to_owned();
    };

    let mut result = String::with_capacity(s.len());
    let mut span_start = 0usize;

    loop {
        result.push_str(&s[span_start..marker]);

        span_start = skip_esc_sequence(s, bytes, marker + 1);

        let Some(next_marker) = find_next_esc(bytes, span_start) else {
            break;
        };
        marker = next_marker;
    }

    result.push_str(&s[span_start..]);
    result
}

fn find_next_esc(bytes: &[u8], start: usize) -> Option<usize> {
    memchr(ESC, &bytes[start..]).map(|relative| start + relative)
}

fn skip_esc_sequence(s: &str, bytes: &[u8], mut cursor: usize) -> usize {
    if cursor >= bytes.len() {
        return cursor;
    }

    match bytes[cursor] {
        b'[' => skip_csi_sequence(bytes, cursor + 1),
        b']' => skip_string_control_sequence(bytes, cursor + 1),
        b'(' | b')' => {
            cursor += 1;
            skip_one_scalar(s, cursor)
        }
        _ => skip_one_scalar(s, cursor),
    }
}

fn skip_one_scalar(s: &str, cursor: usize) -> usize {
    s.get(cursor..)
        .and_then(|tail| tail.chars().next())
        .map_or(cursor, |ch| cursor + ch.len_utf8())
}

fn skip_csi_sequence(bytes: &[u8], mut cursor: usize) -> usize {
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        cursor += 1;
        if (0x40..=0x7E).contains(&byte) {
            break;
        }
    }
    cursor
}

fn skip_string_control_sequence(bytes: &[u8], mut cursor: usize) -> usize {
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\x07' => return cursor + 1,
            ESC => {
                cursor += 1;
                if cursor < bytes.len() && bytes[cursor] == b'\\' {
                    return cursor + 1;
                }
            }
            _ => cursor += 1,
        }
    }
    cursor
}

/// Truncates `s` to at most `max_chars` Unicode scalar values at a char boundary,
/// appending `…` when truncation actually occurs.
///
/// Iterates byte indices rather than collecting the chars so short inputs avoid
/// any allocation and long inputs avoid collecting a full char vector.
pub fn truncate_at_char_boundary(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return if s.is_empty() {
            String::new()
        } else {
            "…".to_string()
        };
    }

    if s.len() <= max_chars {
        return s.to_string();
    }

    if s.as_bytes()[..max_chars].is_ascii() {
        let mut truncated = String::with_capacity(max_chars + '…'.len_utf8());
        truncated.push_str(&s[..max_chars]);
        truncated.push('…');
        return truncated;
    }

    if let Some((truncation_byte_idx, _)) = s.char_indices().nth(max_chars) {
        let mut truncated = String::with_capacity(truncation_byte_idx + '…'.len_utf8());
        truncated.push_str(&s[..truncation_byte_idx]);
        truncated.push('…');
        truncated
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
    fn strip_ansi_codes_preserves_unicode_around_escapes() {
        assert_eq!(
            strip_ansi_codes("pré\x1b[31mfix\x1b[0m 漢字"),
            "préfix 漢字"
        );
    }

    #[test]
    fn strip_ansi_codes_drops_incomplete_escape_at_end() {
        assert_eq!(strip_ansi_codes("partial\x1b["), "partial");
    }

    #[test]
    fn strip_ansi_codes_preserves_c1_controls_without_esc() {
        assert_eq!(
            strip_ansi_codes("\u{009b}31mred\u{009b}0m plain"),
            "\u{009b}31mred\u{009b}0m plain"
        );
    }

    #[test]
    fn strip_ansi_codes_preserves_payload_after_unsupported_esc_sequences() {
        assert_eq!(strip_ansi_codes("\x1bPpayload\x1b\\done"), "payloaddone");
    }

    #[test]
    fn strip_ansi_codes_consumes_full_unicode_scalar_after_single_char_escape() {
        assert_eq!(strip_ansi_codes("\x1béx"), "x");
    }

    #[test]
    fn strip_ansi_codes_consumes_full_unicode_scalar_after_charset_escape() {
        assert_eq!(strip_ansi_codes("\x1b(éx"), "x");
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

    #[test]
    fn truncates_long_ascii_prefix_before_multibyte_tail() {
        let input = "a".repeat(1200) + "€tail";

        let result = truncate_at_char_boundary(&input, 1200);

        assert_eq!(result, format!("{}…", "a".repeat(1200)));
    }
}
