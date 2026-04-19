//! Text helpers shared across tool crates.

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
    use super::truncate_at_char_boundary;

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
