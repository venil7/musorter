//! Album/band consolidation (SPEC "album & band consolidation").
//!
//! Titles that share their common sequence up to a **whitespace boundary**
//! refer to the same entity and all collapse onto the shortest such title:
//!
//! ```text
//! album / album (Japanese edition) / album extra   →  album
//! abc / abc whatever                               →  abc
//! abc / abcde                                      →  DIFFERENT
//! ```

use std::collections::HashMap;

/// Cluster `values` onto canonical representatives.
///
/// Deterministic and order-independent: values are visited shortest-first
/// (byte-length, then bytes); each value is claimed by the first — i.e.
/// shortest — established representative it whitespace-boundarily extends,
/// otherwise it becomes a representative itself. Every value maps to itself
/// when it is canonical.
pub fn cluster<I>(values: I) -> HashMap<String, String>
where
    I: IntoIterator<Item = String>,
{
    let mut uniq: Vec<String> = {
        let set: std::collections::BTreeSet<String> = values.into_iter().collect();
        set.into_iter().collect()
    };
    uniq.sort_by(|a, b| {
        a.len()
            .cmp(&b.len())
            .then_with(|| a.as_bytes().cmp(b.as_bytes()))
    });

    let mut reps: Vec<String> = Vec::new();
    let mut map: HashMap<String, String> = HashMap::new();

    for v in uniq {
        let hit = reps.iter().find(|r| {
            v.len() >= r.len()
                && v.as_bytes()[..r.len()] == r.as_bytes()[..]
                && (v.len() == r.len() || v.as_bytes()[r.len()] == b' ')
        });
        match hit {
            Some(r) => {
                map.insert(v, r.clone());
            }
            None => {
                map.insert(v.clone(), v.clone());
                reps.push(v);
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(values: &[&str]) -> HashMap<String, String> {
        cluster(values.iter().map(|s| s.to_string()))
    }

    fn canon<'a>(map: &'a HashMap<String, String>, v: &'a str) -> &'a str {
        map.get(v).map(String::as_str).unwrap_or(v)
    }

    #[test]
    fn spec_edition_example() {
        let m = c(&["album", "album (Japanese edition)", "album extra"]);
        assert_eq!(canon(&m, "album"), "album");
        assert_eq!(canon(&m, "album (Japanese edition)"), "album");
        assert_eq!(canon(&m, "album extra"), "album");
    }

    #[test]
    fn whitespace_boundary_required() {
        let m = c(&["abc", "abc whatever"]);
        assert_eq!(canon(&m, "abc whatever"), "abc");

        let m = c(&["abc", "abcde"]);
        assert_eq!(canon(&m, "abc"), "abc");
        assert_eq!(canon(&m, "abcde"), "abcde", "'abcde' must stay distinct");
    }

    #[test]
    fn chain_folds_to_shortest() {
        let m = c(&["Album Extra (JP)", "Album Extra", "Album"]);
        assert_eq!(canon(&m, "Album Extra (JP)"), "Album");
        assert_eq!(canon(&m, "Album Extra"), "Album");
        // …and without the shortest member it folds to the next one:
        let m = c(&["Album Extra (JP)", "Album Extra"]);
        assert_eq!(canon(&m, "Album Extra (JP)"), "Album Extra");
    }

    #[test]
    fn order_independent() {
        let a = c(&["band live", "band", "band (deluxe)"]);
        let b = c(&["band (deluxe)", "band live", "band"]);
        assert_eq!(a, b);
    }

    #[test]
    fn distinct_titles_stay_distinct() {
        let m = c(&["Ride The Lightning", "Ride The Lighting"]);
        assert_eq!(canon(&m, "Ride The Lightning"), "Ride The Lightning");
        assert_eq!(canon(&m, "Ride The Lighting"), "Ride The Lighting");
        // single-word different albums never merge
        let m = c(&["Load", "Reload"]);
        assert_eq!(canon(&m, "Load"), "Load");
        assert_eq!(canon(&m, "Reload"), "Reload");
    }

    #[test]
    fn empty_input() {
        assert!(c(&[]).is_empty());
    }
}
