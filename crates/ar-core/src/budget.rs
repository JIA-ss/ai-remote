//! Tool-result budgets. One unbounded tool result can wipe out an agent's context (or be refused by
//! the model API outright), so large text is cut to its head and tail with an explicit count of
//! what was dropped, and images are kept within what model APIs accept.

/// Default total size above which text is cut.
pub const TEXT_LIMIT: usize = 60_000;
/// Characters kept from each end when cutting.
pub const TEXT_KEEP: usize = 20_000;
/// Largest image a result may carry, counted on its base64 text: model APIs reject bigger ones
/// (the Claude API's per-image limit is 5 MB).
pub const IMAGE_MAX_B64: usize = 5 * 1024 * 1024;
/// Longest image side model APIs accept.
pub const IMAGE_MAX_EDGE: u32 = 8000;

/// Cuts `text` to `keep` characters from each end when it exceeds `limit` characters.
pub fn head_tail(text: &str, limit: usize, keep: usize) -> String {
    let total = text.chars().count();
    if total <= limit {
        return text.to_string();
    }
    let head: String = text.chars().take(keep).collect();
    let tail: String = text.chars().skip(total - keep).collect();
    format!("{head}\n\n[... {} characters omitted ...]\n\n{tail}", total - 2 * keep)
}

/// [`head_tail`] with the default limits.
pub fn clip(text: &str) -> String {
    head_tail(text, TEXT_LIMIT, TEXT_KEEP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_unchanged() {
        assert_eq!(head_tail("hello", 10, 2), "hello");
    }

    #[test]
    fn long_text_keeps_both_ends() {
        let out = head_tail("abcdefghijkl", 10, 3);
        assert!(out.starts_with("abc"));
        assert!(out.ends_with("jkl"));
        assert!(out.contains("6 characters omitted"));
    }

    #[test]
    fn counts_characters_not_bytes() {
        let text = "世".repeat(30);
        let out = head_tail(&text, 20, 5);
        assert!(out.contains("20 characters omitted"));
    }
}
