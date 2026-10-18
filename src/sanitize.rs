//! Path segment sanitization (SPEC "path sanitization"): make any rendered
//! segment safe on every platform Rust targets.

const MAX_SEGMENT: usize = 150;

const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Sanitize a single path segment (no `/`). Returns the safe segment, which
/// may be empty (caller decides how to handle that).
pub fn segment(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| {
            if c == '/'
                || c == '\\'
                || c == ':'
                || c == '*'
                || c == '?'
                || c == '"'
                || c == '<'
                || c == '>'
                || c == '|'
                || c.is_control()
            {
                ' '
            } else {
                c
            }
        })
        .collect();

    let collapsed = collapse_ws(&replaced);
    let trimmed = trim_dots_spaces(&collapsed);
    let truncated = truncate(&trimmed);
    let final_ = trim_dots_spaces(&truncated);
    if final_.is_empty() {
        return String::new();
    }
    guard_reserved(&final_)
}

/// Sanitize a file stem (segment rules apply; `.` inside is preserved).
pub fn file_stem(raw: &str) -> String {
    segment(raw)
}

/// Keep the extension verbatim but strip anything path-hostile from it.
pub fn extension(raw: &str) -> String {
    let s: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>| ".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    s.trim_matches('.').to_string()
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    for c in s.chars() {
        if c == ' ' {
            if !last_space && !out.is_empty() {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out
}

fn trim_dots_spaces(s: &str) -> String {
    s.trim().trim_matches('.').trim().to_string()
}

/// UTF-8-safe truncation to MAX_SEGMENT bytes.
fn truncate(s: &str) -> String {
    if s.len() <= MAX_SEGMENT {
        return s.to_string();
    }
    let mut end = MAX_SEGMENT;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Windows reserved device names (with or without an extension) get a `_`.
fn guard_reserved(s: &str) -> String {
    let base = s.split('.').next().unwrap_or(s);
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(base)) {
        format!("_{s}")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_chars_replaced() {
        assert_eq!(segment("a<b>c:d\"e/f\\g|h?i*j"), "a b c d e f g h i j");
        assert_eq!(segment("col:on"), "col on");
    }

    #[test]
    fn control_chars_removed() {
        assert_eq!(segment("a\u{0}b\u{7f}c"), "a b c");
    }

    #[test]
    fn trims_dots_and_spaces() {
        assert_eq!(segment("  .hidden.  "), "hidden");
        assert_eq!(segment("..."), "");
        assert_eq!(segment("name."), "name");
    }

    #[test]
    fn reserved_names() {
        assert_eq!(segment("CON"), "_CON");
        assert_eq!(segment("con.mp3"), "_con.mp3");
        assert_eq!(segment("COM1"), "_COM1");
        assert_eq!(segment("Console"), "Console");
        assert_eq!(segment("lpt9.txt"), "_lpt9.txt");
    }

    #[test]
    fn truncates_utf8_safely() {
        let long = "é".repeat(200); // 2 bytes each
        let out = segment(&long);
        assert!(out.len() <= MAX_SEGMENT);
        assert!(out.chars().all(|c| c == 'é'));
    }

    #[test]
    fn preserves_internal_dots_and_unicode() {
        assert_eq!(segment("Mr. Brightside.mp3"), "Mr. Brightside.mp3");
        assert_eq!(segment("Sigur Rós – Sæglópur"), "Sigur Rós – Sæglópur");
    }

    #[test]
    fn extension_sanitization() {
        assert_eq!(extension("mp3"), "mp3");
        assert_eq!(extension("MP3"), "MP3");
        assert_eq!(extension("a b"), "a_b");
    }
}
