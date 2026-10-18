//! Path/filename metadata inference (SPEC "meta data sources" #3): fills
//! fields that are still missing after overrides and tags.
//!
//! Heuristics (only ever *fill*, never override):
//! - file stem `NN - Title` / `NN. Title` / `NN_Title` / … → track + title,
//!   otherwise the whole stem → title
//! - nearest year-ish ancestor (`1985`, `1985 - Live`, `(1985)`) → year
//!   (+ album from the remainder when present)
//! - outermost remaining ancestor → artist, next → album
//!   (single-dir layout `Band/track.mp3` ⇒ band only, album stays unknown)

use crate::meta::normalize::{leading_year, split_track_title};
use crate::model::MetaKey;
use std::collections::HashMap;
use std::path::Path;

pub fn infer(rel: &Path) -> HashMap<MetaKey, String> {
    let mut m = HashMap::new();

    let mut comps: Vec<String> = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(str::to_string))
        .collect();
    let file = comps.pop();

    // --- file name → track / title ---
    if let Some(name) = file {
        let stem = Path::new(&name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(name.as_str());
        let (track, title) = split_track_title(stem);
        if let Some(t) = track {
            m.insert(MetaKey::Track, t);
        }
        if let Some(t) = title {
            m.insert(MetaKey::Title, t);
        }
    }

    // --- ancestor dirs ---
    // Outer-most first (root → file), as stored in `comps`.
    // 1) find the nearest year-ish dir (nearest = last in outer-first order)
    let mut year_idx: Option<usize> = None;
    let mut year_val: Option<String> = None;
    let mut album_val: Option<String> = None;
    for (i, d) in comps.iter().enumerate().rev() {
        if let Some((y, rest)) = split_yearish(d) {
            year_idx = Some(i);
            year_val = Some(y);
            if !rest.is_empty() {
                album_val = Some(rest);
            }
            break;
        }
    }
    if let Some(y) = year_val {
        m.insert(MetaKey::Year, y);
    }
    if let Some(a) = &album_val {
        m.insert(MetaKey::Album, a.clone());
    }

    // 2) remaining dirs, outer-most first → artist, then album
    let remaining: Vec<&String> = comps
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != year_idx)
        .map(|(_, d)| d)
        .collect();
    if !remaining.is_empty() && !m.contains_key(&MetaKey::Artist) {
        m.insert(MetaKey::Artist, remaining[0].clone());
    }
    if remaining.len() >= 2 && album_val.is_none() {
        m.insert(MetaKey::Album, remaining[1].clone());
    }

    m
}

/// `1985` → (1985, ""); `1985 - Live` → (1985, Live); `(1985)` → (1985, "");
/// `(1985) Live` → (1985, Live). Returns None when not year-ish.
fn split_yearish(dir: &str) -> Option<(String, String)> {
    let s = dir.trim();
    if s.is_empty() {
        return None;
    }

    // Parenthesised year at the start: `(1985)` / `(1985) Rest`
    if s.starts_with('(') {
        let end = s.find(')')?;
        let inner = &s[1..end];
        if let Some(y) = leading_year(inner)
            && inner.chars().all(|c| c.is_ascii_digit())
        {
            let rest = strip_sep(&s[end + 1..]);
            return Some((y, rest));
        }
        return None;
    }

    // Plain 4-digit year at the start; must be followed by end or a separator
    // (so `19851` or `1985x` is not year-ish).
    let bytes = s.as_bytes();
    if bytes.len() < 4 || !s.is_char_boundary(4) {
        return None;
    }
    let head = &s[..4];
    if leading_year(head)? != head {
        return None;
    }
    let rest_raw = &s[4..];
    if rest_raw.is_empty() {
        return Some((head.to_string(), String::new()));
    }
    let first = rest_raw.chars().next().unwrap();
    if matches!(first, '-' | ' ' | '_' | '(' | '[') {
        let rest = strip_sep(rest_raw);
        return Some((head.to_string(), rest));
    }
    None
}

/// Strip leading/trailing separator noise from an album remainder.
fn strip_sep(s: &str) -> String {
    s.trim()
        .trim_matches(|c| matches!(c, '-' | '_' | ' ' | '[' | ']' | '(' | ')'))
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn inf(p: &str) -> HashMap<MetaKey, String> {
        infer(&PathBuf::from(p))
    }

    fn get(m: &HashMap<MetaKey, String>, k: MetaKey) -> Option<&str> {
        m.get(&k).map(String::as_str)
    }

    #[test]
    fn artist_album_track_layout() {
        let m = inf("Metallica/Master OF PUPPETS/1985/01 - Master of Puppets.mp3");
        assert_eq!(get(&m, MetaKey::Artist), Some("Metallica"));
        assert_eq!(get(&m, MetaKey::Album), Some("Master OF PUPPETS"));
        assert_eq!(get(&m, MetaKey::Year), Some("1985"));
        assert_eq!(get(&m, MetaKey::Track), Some("01"));
        assert_eq!(get(&m, MetaKey::Title), Some("Master of Puppets"));
    }

    #[test]
    fn two_dir_layout() {
        let m = inf("metallica/BlackAlbum/song.mp3");
        assert_eq!(get(&m, MetaKey::Artist), Some("metallica"));
        assert_eq!(get(&m, MetaKey::Album), Some("BlackAlbum"));
        assert_eq!(get(&m, MetaKey::Year), None);
    }

    #[test]
    fn single_dir_is_artist_not_album() {
        let m = inf("Metallica/song.mp3");
        assert_eq!(get(&m, MetaKey::Artist), Some("Metallica"));
        assert_eq!(get(&m, MetaKey::Album), None);
    }

    #[test]
    fn artist_year_layout() {
        let m = inf("Metallica/1985/song.mp3");
        assert_eq!(get(&m, MetaKey::Artist), Some("Metallica"));
        assert_eq!(get(&m, MetaKey::Year), Some("1985"));
        assert_eq!(get(&m, MetaKey::Album), None);
    }

    #[test]
    fn year_dir_with_remainder_is_album() {
        let m = inf("Metallica/1985 - Live/song.mp3");
        assert_eq!(get(&m, MetaKey::Artist), Some("Metallica"));
        assert_eq!(get(&m, MetaKey::Year), Some("1985"));
        assert_eq!(get(&m, MetaKey::Album), Some("Live"));
    }

    #[test]
    fn parenthesised_year_dir() {
        let m = inf("Band/(1999) Album/song.flac");
        assert_eq!(get(&m, MetaKey::Artist), Some("Band"));
        assert_eq!(get(&m, MetaKey::Year), Some("1999"));
        assert_eq!(get(&m, MetaKey::Album), Some("Album"));
    }

    #[test]
    fn filename_without_track() {
        let m = inf("Band/Album/Intro.mp3");
        assert_eq!(get(&m, MetaKey::Title), Some("Intro"));
        assert_eq!(get(&m, MetaKey::Track), None);
    }

    #[test]
    fn non_year_dirs_not_mistaken() {
        // "19856 remix" is not a year dir
        let m = inf("Band/19856 remix/song.mp3");
        assert_eq!(get(&m, MetaKey::Year), None);
        assert_eq!(get(&m, MetaKey::Artist), Some("Band"));
        assert_eq!(get(&m, MetaKey::Album), Some("19856 remix"));
    }

    #[test]
    fn year_split_helpers() {
        assert_eq!(split_yearish("1985"), Some(("1985".into(), "".into())));
        assert_eq!(
            split_yearish("1985 - Live"),
            Some(("1985".into(), "Live".into()))
        );
        assert_eq!(split_yearish("(1985)"), Some(("1985".into(), "".into())));
        assert_eq!(
            split_yearish("(1985) Live"),
            Some(("1985".into(), "Live".into()))
        );
        assert_eq!(split_yearish("Greatest Hits"), None);
        assert_eq!(split_yearish("19851"), None);
        assert_eq!(split_yearish("1985x"), None);
    }
}
