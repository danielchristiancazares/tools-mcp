/// Truncate a string by Unicode scalar value count and append an ellipsis when needed.
#[must_use]
pub fn truncate_chars_with_ellipsis(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return "…".to_string();
    }

    let mut char_count = 0usize;
    let mut truncation_byte_idx = input.len();

    for (byte_idx, _) in input.char_indices() {
        if char_count == max_chars {
            truncation_byte_idx = byte_idx;
            break;
        }
        char_count += 1;
    }

    if char_count == max_chars && truncation_byte_idx < input.len() {
        format!("{}…", &input[..truncation_byte_idx])
    } else {
        input.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::truncate_chars_with_ellipsis;

    #[test]
    fn truncate_chars_with_ellipsis_truncates_ascii() {
        let out = truncate_chars_with_ellipsis("abcdef", 3);
        assert_eq!(out, "abc…");
    }

    #[test]
    fn truncate_chars_with_ellipsis_preserves_short_input() {
        let out = truncate_chars_with_ellipsis("abc", 10);
        assert_eq!(out, "abc");
    }

    #[test]
    fn truncate_chars_with_ellipsis_handles_unicode_boundaries() {
        let out = truncate_chars_with_ellipsis("éééé", 3);
        assert_eq!(out, "ééé…");
    }

    #[test]
    fn truncate_chars_with_ellipsis_preserves_exact_limit() {
        let out = truncate_chars_with_ellipsis("ééé", 3);
        assert_eq!(out, "ééé");
    }

    #[test]
    fn truncate_chars_with_ellipsis_returns_only_ellipsis_for_zero_limit() {
        let out = truncate_chars_with_ellipsis("abcdef", 0);
        assert_eq!(out, "…");
    }
}
