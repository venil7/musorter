//! In-memory meta data normalization (SPEC "meta data normalization").
//!
//! Pipeline: NFC → drop controls → `_`→space → split into words (whitespace,
//! hyphens, case transitions, letter↔digit transitions) → title-case each
//! word → collapse/trim. Idempotent: `normalize(normalize(x)) == normalize(x)`.

use unicode_normalization::UnicodeNormalization;

pub fn normalize(raw: &str) -> String {
    let cleaned: String = raw
        .nfc()
        .filter(|c| !c.is_control())
        .map(|c| if c == '_' { ' ' } else { c })
        .collect();

    let mut out = String::new();
    let mut seg = String::new();
    let mut prev: Option<char> = None;

    for c in cleaned.chars() {
        if c == '-' {
            flush(&mut seg, &mut out);
            push_sep(&mut out, '-');
            prev = Some(c);
            continue;
        }
        if c.is_whitespace() {
            flush(&mut seg, &mut out);
            push_sep(&mut out, ' ');
            prev = Some(c);
            continue;
        }
        let boundary = prev.is_some_and(|p| {
            (p.is_lowercase() && c.is_uppercase())
                || (p.is_alphabetic() && c.is_numeric())
                || (p.is_numeric() && c.is_alphabetic())
        });
        if boundary && !seg.is_empty() {
            flush(&mut seg, &mut out);
            push_sep(&mut out, ' ');
        }
        seg.push(c);
        prev = Some(c);
    }
    flush(&mut seg, &mut out);

    out.trim()
        .trim_matches(|c| c == '-' || c == ' ')
        .to_string()
}

/// Title-case a single word: first alphabetic char uppercased (apostrophes
/// and other non-alphas skipped when finding it), the rest lowercased.
fn title_word(w: &str) -> String {
    let mut res = String::with_capacity(w.len());
    let mut first_alpha = true;
    for c in w.chars() {
        if c.is_alphabetic() {
            if first_alpha {
                res.extend(c.to_uppercase());
                first_alpha = false;
            } else {
                res.extend(c.to_lowercase());
            }
        } else {
            res.push(c);
        }
    }
    res
}

fn flush(seg: &mut String, out: &mut String) {
    if !seg.is_empty() {
        out.push_str(&title_word(seg));
        seg.clear();
    }
}

/// Push a separator, collapsing runs. Design choices:
/// - a *spaced* hyphen (`Machine - Head`) degrades to a plain space
///   (`Machine Head`) so spelling variants land in the same folder;
/// - an *attached* hyphen (`Kill-'Em-All`) is kept verbatim (SPEC).
fn push_sep(out: &mut String, sep: char) {
    if out.is_empty() {
        return;
    }
    if sep == ' ' {
        while out.ends_with(' ') || out.ends_with('-') {
            out.pop();
        }
        out.push(' ');
    } else {
        // '-': keep only when attached (previous char is not a separator).
        if out.ends_with(' ') || out.ends_with('-') {
            return;
        }
        out.push('-');
    }
}

/// Extract a track number and title from a file stem
/// (`NN - Title`, `NN. Title`, `NN_Title`, `NN-Title`, `NN Title`).
pub fn split_track_title(stem: &str) -> (Option<String>, Option<String>) {
    let s = stem.trim();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() && chars[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && i <= 3 && i < chars.len() && matches!(chars[i], ' ' | '.' | '_' | '-') {
        // Strip separator characters (at least one) before the title.
        let mut j = i;
        while j < chars.len() && matches!(chars[j], ' ' | '.' | '_' | '-') {
            j += 1;
        }
        if j < chars.len() {
            let track: String = chars[..i].iter().collect();
            let title: String = chars[j..].iter().collect();
            let title = title.trim();
            if !title.is_empty() {
                return (Some(track), Some(title.to_string()));
            }
        }
    }
    (None, (!s.is_empty()).then(|| s.to_string()))
}

/// Leading-4-digits year of a date-ish string (`1985`, `1985-05-01`, `(1985)`).
pub fn leading_year(s: &str) -> Option<String> {
    let digits: String = s
        .trim()
        .trim_start_matches('(')
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .take(4)
        .collect();
    (digits.len() == 4 && (1000..=2999).contains(&digits.parse::<u32>().ok()?)).then_some(digits)
}

/// Zero-pad a track number to at least 2 digits for `$track` rendering.
pub fn format_track(raw: &str) -> Option<String> {
    let digits: String = raw.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let n: u32 = digits.parse().ok()?;
    Some(format!("{n:02}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> String {
        normalize(s)
    }

    #[test]
    fn spec_examples() {
        assert_eq!(n("Master OF PUPPETS"), "Master Of Puppets");
        assert_eq!(n("METALLICA"), "Metallica");
        assert_eq!(n("BlackAlbum"), "Black Album");
        assert_eq!(n("RideTheLighting"), "Ride The Lighting");
        // hyphens are kept when attached
        assert_eq!(n("Kill-'Em-All"), "Kill-'Em-All");
        assert_eq!(n("_the_black_"), "The Black");
        // spaced hyphens collapse into a space
        assert_eq!(n("Machine - Head"), "Machine Head");
        assert_eq!(n("AC - DC"), "Ac Dc");
    }

    #[test]
    fn apostrophes_and_case() {
        assert_eq!(n("'em"), "'Em");
        assert_eq!(n("don't"), "Don't");
        assert_eq!(n("OF"), "Of");
        assert_eq!(n("IT'S"), "It's");
    }

    #[test]
    fn digits() {
        assert_eq!(n("1985"), "1985");
        assert_eq!(n("Volume2"), "Volume 2");
        assert_eq!(n("2 volumes"), "2 Volumes");
    }

    #[test]
    fn trims_and_collapses() {
        assert_eq!(n("  spaced   out  "), "Spaced Out");
        assert_eq!(n("a -- b"), "A B");
        assert_eq!(n("--leading"), "Leading");
        assert_eq!(n("trailing--"), "Trailing");
        assert_eq!(n("under_score_case"), "Under Score Case");
    }

    #[test]
    fn unicode_kept() {
        assert_eq!(n("sigur rós"), "Sigur Rós");
        assert_eq!(n("МОСКВА"), "Москва");
        assert_eq!(n("кассета 1985"), "Кассета 1985");
    }

    #[test]
    fn idempotent() {
        for s in [
            "Master OF PUPPETS",
            "Kill-'Em-All",
            "Machine - Head",
            "BlackAlbum",
            "  a -- b ",
            "'em",
            "Volume2",
            "AC/DC",
        ] {
            let once = n(s);
            assert_eq!(once, n(&once), "not idempotent for {s:?}");
        }
    }

    #[test]
    fn track_title_split() {
        assert_eq!(
            split_track_title("01 - Master of Puppets"),
            (Some("01".into()), Some("Master of Puppets".into()))
        );
        assert_eq!(
            split_track_title("3. One"),
            (Some("3".into()), Some("One".into()))
        );
        assert_eq!(
            split_track_title("07_Two"),
            (Some("07".into()), Some("Two".into()))
        );
        assert_eq!(
            split_track_title("012 envious"),
            (Some("012".into()), Some("envious".into()))
        );
        assert_eq!(split_track_title("Intro"), (None, Some("Intro".into())));
        assert_eq!(split_track_title("1985"), (None, Some("1985".into())));
    }

    #[test]
    fn year_extraction() {
        assert_eq!(leading_year("1985"), Some("1985".into()));
        assert_eq!(leading_year("1985-05-01"), Some("1985".into()));
        assert_eq!(leading_year("(1985)"), Some("1985".into()));
        assert_eq!(leading_year("999"), None);
        assert_eq!(leading_year("0000"), None);
        assert_eq!(leading_year("nope"), None);
    }

    #[test]
    fn track_formatting() {
        assert_eq!(format_track("3"), Some("03".into()));
        assert_eq!(format_track("01"), Some("01".into()));
        assert_eq!(format_track("3/12"), Some("03".into()));
        assert_eq!(format_track("123"), Some("123".into()));
        assert_eq!(format_track("abc"), None);
    }
}
