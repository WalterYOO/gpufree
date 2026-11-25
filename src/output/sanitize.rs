//! Strip terminal control characters from remote-origin strings (A26).
//!
//! Hostnames, GPU names, process names and error text come from a remote
//! machine. Rendering them verbatim lets that machine move the cursor, retitle
//! the terminal, or worse via OSC sequences. JSON keeps the raw value (serde
//! escapes control characters); the table and TUI call [`clean`] first.

/// Replacement for every C0 (0x00–0x1F), DEL (0x7F) and C1 (0x80–0x9F)
/// character: a visible, width-1 glyph.
const REPLACEMENT: char = '\u{fffd}';

pub fn is_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}')
}

/// Return `text` with control characters replaced. Non-control text passes
/// through unchanged.
pub fn clean(text: &str) -> String {
    if !text.chars().any(is_control) {
        return text.to_string();
    }
    text.chars()
        .map(|c| if is_control(c) { REPLACEMENT } else { c })
        .collect()
}

/// Convenience for optional remote fields: `None` renders as `fallback`.
pub fn clean_or(text: Option<&str>, fallback: &str) -> String {
    match text {
        Some(value) => clean(value),
        None => fallback.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_c0_c1_and_del() {
        assert_eq!(clean("plain"), "plain");
        assert_eq!(clean("a\u{1b}]0;PWNED\u{7}b"), "a\u{fffd}]0;PWNED\u{fffd}b");
        assert_eq!(clean("bell\u{7}"), "bell\u{fffd}");
        assert_eq!(clean("del\u{7f}"), "del\u{fffd}");
        assert_eq!(clean("c1\u{9b}31m"), "c1\u{fffd}31m");
        assert_eq!(clean("new\nline"), "new\u{fffd}line");
    }

    #[test]
    fn leaves_unicode_text_alone() {
        assert_eq!(clean("NVIDIA H20-3e · 张伟"), "NVIDIA H20-3e · 张伟");
    }

    #[test]
    fn clean_or_uses_fallback_for_none() {
        assert_eq!(clean_or(None, "—"), "—");
        assert_eq!(clean_or(Some("a\u{1b}"), "—"), "a\u{fffd}");
    }
}
