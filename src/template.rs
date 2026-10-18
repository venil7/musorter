//! Output path template: parse to a section/part AST and render per file.
//!
//! Grammar: `/`-separated sections; each section is literal text with
//! `$var` placeholders (`$$` = literal `$`). The last section is the file
//! name and must either reference `$ext` or carry a literal extension.
//!
//! Missing-data rules (SPEC):
//! - a **critical** var (`$band`, `$album`, `$title`) without a value ⇒ the
//!   whole file is unprocessed (`Err(critical)`);
//! - a missing **optional** var is dropped together with its adjacent ` - `
//!   glue, leaving no artifacts;
//! - a section that renders empty is dropped entirely.

use crate::model::{MetaKey, ResolvedMeta, ScannedFile};
use crate::sanitize;
use std::path::{Path, PathBuf};

pub const DEFAULT_TEMPLATE: &str = "{$band}/{$year - $album}/{$track - $title}.{$ext}";

#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Lit(String),
    Var(MetaKey),
}

#[derive(Debug, Clone)]
pub struct Template {
    pub sections: Vec<Vec<Part>>,
}

impl Template {
    pub fn parse(spec: &str) -> Result<Template, String> {
        if spec.trim().is_empty() {
            return Err("template must not be empty".into());
        }
        // Braces are purely notational grouping (`{$year - $album}` wraps a
        // multi-variable section) — strip them before scanning for `$vars`.
        let stripped: String = spec.chars().filter(|c| *c != '{' && *c != '}').collect();
        let mut sections = Vec::new();
        for raw in stripped.split('/') {
            if raw.is_empty() {
                return Err(format!("empty path section in template {spec:?}"));
            }
            let chars: Vec<char> = raw.chars().collect();
            let mut parts: Vec<Part> = Vec::new();
            let mut lit = String::new();
            let mut i = 0;
            while i < chars.len() {
                if chars[i] == '$' {
                    if chars.get(i + 1) == Some(&'$') {
                        lit.push('$'); // `$$` escape → literal `$`
                        i += 2;
                        continue;
                    }
                    let start = i + 1;
                    let mut j = start;
                    while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                        j += 1;
                    }
                    let valid =
                        j > start && (chars[start].is_ascii_alphabetic() || chars[start] == '_');
                    if !valid {
                        return Err(format!("dangling '$' in template {spec:?}"));
                    }
                    let name: String = chars[start..j].iter().collect();
                    let key = MetaKey::from_name(&name)
                        .ok_or_else(|| format!("unknown template variable ${name}"))?;
                    if !lit.is_empty() {
                        parts.push(Part::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(Part::Var(key));
                    i = j;
                } else {
                    lit.push(chars[i]);
                    i += 1;
                }
            }
            if !lit.is_empty() {
                parts.push(Part::Lit(lit));
            }
            if parts.is_empty() {
                return Err(format!("empty section in template {spec:?}"));
            }
            sections.push(parts);
        }

        // The file-name section must produce an extension.
        let last = sections.last().expect("non-empty sections");
        let has_ext = last.iter().any(|p| matches!(p, Part::Var(MetaKey::Ext)));
        if !has_ext {
            let literal_tail = last
                .iter()
                .filter_map(|p| match p {
                    Part::Lit(s) => Some(s.as_str()),
                    _ => None,
                })
                .next_back()
                .unwrap_or("");
            let ok = literal_tail
                .rsplit_once('.')
                .is_some_and(|(_, ext)| !ext.trim().is_empty());
            if !ok {
                return Err(
                    "template file-name section must reference $ext or include a literal \
                     extension (e.g. `.mp3`)"
                        .into(),
                );
            }
        }

        Ok(Template { sections })
    }

    /// Directory sections (all but the last) — used for album art.
    pub fn dir_sections(&self) -> &[Vec<Part>] {
        &self.sections[..self.sections.len() - 1]
    }

    pub fn render(
        &self,
        out: &Path,
        file: &ScannedFile,
        meta: Option<&ResolvedMeta>,
    ) -> Result<PathBuf, MetaKey> {
        render_sections(&self.sections, out, file, meta)
    }

    pub fn render_dir_sections(
        &self,
        out: &Path,
        file: &ScannedFile,
        meta: Option<&ResolvedMeta>,
    ) -> Result<PathBuf, MetaKey> {
        render_sections(self.dir_sections(), out, file, meta)
    }
}

fn lookup(meta: Option<&ResolvedMeta>, file: &ScannedFile, key: MetaKey) -> Option<String> {
    if key == MetaKey::Ext {
        return (!file.ext.is_empty()).then(|| file.ext.clone());
    }
    let m = meta?;
    let v = if key == MetaKey::Band {
        m.band()?.to_string()
    } else if key == MetaKey::Track {
        let raw = m.get(key)?;
        crate::meta::normalize::format_track(raw).unwrap_or_else(|| raw.to_string())
    } else {
        m.get(key)?.to_string()
    };
    (!v.is_empty()).then_some(v)
}

/// Check the critical vars used by `sections`; `None` = all good.
fn first_missing_critical(
    sections: &[Vec<Part>],
    file: &ScannedFile,
    meta: Option<&ResolvedMeta>,
    is_image: bool,
) -> Option<MetaKey> {
    for parts in sections {
        for p in parts {
            if let Part::Var(k) = p {
                if !k.is_critical() {
                    continue;
                }
                if is_image && *k == MetaKey::Title {
                    continue; // album art keeps its own file name
                }
                if lookup(meta, file, *k).is_none() {
                    return Some(*k);
                }
            }
        }
    }
    None
}

fn render_sections(
    sections: &[Vec<Part>],
    out: &Path,
    file: &ScannedFile,
    meta: Option<&ResolvedMeta>,
) -> Result<PathBuf, MetaKey> {
    let is_image = file.class == crate::model::FileClass::Image;
    if let Some(k) = first_missing_critical(sections, file, meta, is_image) {
        return Err(k);
    }

    let mut rel = PathBuf::new();
    for (i, parts) in sections.iter().enumerate() {
        let last = i + 1 == sections.len();
        let rendered = render_section(parts, |k| lookup(meta, file, k));
        if last {
            rel.push(render_filename(rendered.unwrap_or_default()));
        } else if let Some(seg) = rendered {
            let safe = sanitize::segment(&seg);
            if !safe.is_empty() {
                rel.push(safe);
            }
            // empty ⇒ section dropped (no empty folders)
        }
    }
    Ok(out.join(rel))
}

/// Render one section. Returns `None` when nothing but optional glue is left.
fn render_section(
    parts: &[Part],
    mut val: impl FnMut(MetaKey) -> Option<String>,
) -> Option<String> {
    enum Cell {
        Lit(String),
        Val(String),
        Absent,
    }
    let cells: Vec<Cell> = parts
        .iter()
        .map(|p| match p {
            Part::Lit(s) => Cell::Lit(s.clone()),
            Part::Var(k) => match val(*k) {
                Some(v) => Cell::Val(v),
                None => Cell::Absent,
            },
        })
        .collect();

    let mut out = String::new();
    for (i, cell) in cells.iter().enumerate() {
        match cell {
            Cell::Lit(s) => {
                let mut s = s.clone();
                // Adjacent glue handling around absent vars.
                if matches!(cells.get(i + 1), Some(Cell::Absent)) {
                    s = strip_trailing_glue(&s);
                }
                if i > 0 && matches!(cells.get(i - 1), Some(Cell::Absent)) {
                    s = strip_leading_glue(&s);
                }
                out.push_str(&s);
            }
            Cell::Val(v) => out.push_str(v),
            Cell::Absent => {}
        }
    }

    let trimmed = out.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// `Live - ` + absent ⇒ `Live`; `Super` unchanged (no trailing `-`).
fn strip_trailing_glue(s: &str) -> String {
    let t = s.trim_end();
    match t.strip_suffix('-') {
        Some(rest) => rest.trim_end().to_string(),
        None => s.to_string(),
    }
}

/// ` - ` + absent ⇒ ``; ` - Bonus` ⇒ `Bonus`.
fn strip_leading_glue(s: &str) -> String {
    let t = s.trim_start();
    match t.strip_prefix('-') {
        Some(rest) => rest.trim_start().to_string(),
        None => s.to_string(),
    }
}

/// Split a rendered file-name section at the last `.`, sanitize both halves
/// and (re-attach the extension; an empty stem becomes `_`.
fn render_filename(rendered: String) -> String {
    match rendered.rsplit_once('.') {
        Some((stem, ext)) => {
            let stem = sanitize::file_stem(stem);
            let stem = if stem.is_empty() {
                "_".to_string()
            } else {
                stem
            };
            let ext = sanitize::extension(ext);
            if ext.is_empty() {
                stem
            } else {
                format!("{stem}.{ext}")
            }
        }
        None => {
            let s = sanitize::file_stem(&rendered);
            if s.is_empty() { "_".to_string() } else { s }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileClass, Source};

    fn file(ext: &str, class: FileClass) -> ScannedFile {
        ScannedFile {
            src: PathBuf::from("/in/x"),
            rel: PathBuf::from("x"),
            class,
            ext: ext.to_string(),
            size: 1,
            symlink: false,
        }
    }

    fn meta(pairs: &[(MetaKey, &str)]) -> ResolvedMeta {
        let mut m = ResolvedMeta::default();
        for (k, v) in pairs {
            m.put(*k, v.to_string(), Source::Tag);
        }
        m
    }

    fn r(tpl: &str, pairs: &[(MetaKey, &str)], ext: &str) -> Result<String, MetaKey> {
        let t = Template::parse(tpl).unwrap();
        let f = file(ext, FileClass::Audio);
        let m = meta(pairs);
        let path = t.render(Path::new("/out"), &f, Some(&m))?;
        Ok(path.to_string_lossy().into_owned())
    }

    #[test]
    fn default_template_full() {
        let got = r(
            DEFAULT_TEMPLATE,
            &[
                (MetaKey::Band, "Metallica"),
                (MetaKey::Year, "1986"),
                (MetaKey::Album, "Master of Puppets"),
                (MetaKey::Track, "3"),
                (MetaKey::Title, "Master of Puppets"),
            ],
            "mp3",
        )
        .unwrap();
        assert_eq!(
            got,
            "/out/Metallica/1986 - Master of Puppets/03 - Master of Puppets.mp3"
        );
    }

    #[test]
    fn missing_year_collapses_without_artifact() {
        let got = r(
            DEFAULT_TEMPLATE,
            &[
                (MetaKey::Band, "Metallica"),
                (MetaKey::Album, "Black Album"),
                (MetaKey::Title, "Sad but True"),
            ],
            "flac",
        )
        .unwrap();
        assert_eq!(got, "/out/Metallica/Black Album/Sad but True.flac");
    }

    #[test]
    fn missing_track_collapses_without_artifact() {
        let got = r(
            DEFAULT_TEMPLATE,
            &[
                (MetaKey::Band, "Band"),
                (MetaKey::Year, "2000"),
                (MetaKey::Album, "Album"),
                (MetaKey::Title, "Intro"),
            ],
            "mp3",
        )
        .unwrap();
        assert_eq!(got, "/out/Band/2000 - Album/Intro.mp3");
    }

    #[test]
    fn critical_missing() {
        assert_eq!(
            r(DEFAULT_TEMPLATE, &[(MetaKey::Title, "x")], "mp3"),
            Err(MetaKey::Band)
        );
        assert_eq!(
            r(
                DEFAULT_TEMPLATE,
                &[(MetaKey::Band, "b"), (MetaKey::Title, "x")],
                "mp3"
            ),
            Err(MetaKey::Album)
        );
        assert_eq!(
            r(
                DEFAULT_TEMPLATE,
                &[(MetaKey::Band, "b"), (MetaKey::Album, "a")],
                "mp3"
            ),
            Err(MetaKey::Title)
        );
    }

    #[test]
    fn critical_only_when_used() {
        // No $album in the template ⇒ missing album is fine.
        let got = r("{$band}/{$title}.{$ext}", &[(MetaKey::Band, "B")], "mp3");
        assert_eq!(got, Err(MetaKey::Title)); // title still critical & missing
        let got = r(
            "{$band}/{$title}.{$ext}",
            &[(MetaKey::Band, "B"), (MetaKey::Title, "T")],
            "mp3",
        );
        assert_eq!(got.unwrap(), "/out/B/T.mp3");
    }

    #[test]
    fn empty_section_dropped() {
        // band + album only (no year) — no empty dir even with glue.
        let got = r(
            "{$band}/{$year - $album}/{$title}.{$ext}",
            &[
                (MetaKey::Band, "B"),
                (MetaKey::Album, "A"),
                (MetaKey::Title, "T"),
            ],
            "ogg",
        )
        .unwrap();
        assert_eq!(got, "/out/B/A/T.ogg");
    }

    #[test]
    fn sanitization_applied_per_segment() {
        let got = r(
            DEFAULT_TEMPLATE,
            &[
                (MetaKey::Band, "AC/DC"),
                (MetaKey::Year, "1985"),
                (MetaKey::Album, "Back in Black"),
                (MetaKey::Track, "1"),
                (MetaKey::Title, "A<NT>:Back?In*Black"),
            ],
            "mp3",
        )
        .unwrap();
        assert_eq!(
            got,
            "/out/AC DC/1985 - Back in Black/01 - A NT Back In Black.mp3"
        );
    }

    #[test]
    fn unknown_variable_is_error() {
        assert_eq!(
            Template::parse("{$band}/{$nope}").unwrap_err(),
            "unknown template variable $nope"
        );
    }

    #[test]
    fn filename_validation() {
        assert!(Template::parse("{$band}/{$title}.mp3").is_ok());
        assert!(Template::parse("{$band}/{$title}").is_err());
        assert!(Template::parse("{$band}/{$title}.{$ext}").is_ok());
    }

    #[test]
    fn literal_dollar_escape() {
        let t = Template::parse("{$band}/Cash $$ {$title}.{$ext}").unwrap();
        let f = file("mp3", FileClass::Audio);
        let m = meta(&[(MetaKey::Band, "Cash"), (MetaKey::Title, "Song")]);
        let p = t.render(Path::new("/o"), &f, Some(&m)).unwrap();
        assert_eq!(p.to_string_lossy(), "/o/Cash/Cash $ Song.mp3");
    }

    #[test]
    fn image_skips_title_critical() {
        let t = Template::parse(DEFAULT_TEMPLATE).unwrap();
        let f = file("jpg", FileClass::Image);
        let m = meta(&[(MetaKey::Band, "B"), (MetaKey::Album, "A")]);
        // directory sections only ⇒ no title required
        let p = t
            .render_dir_sections(Path::new("/out"), &f, Some(&m))
            .unwrap();
        assert_eq!(p.to_string_lossy(), "/out/B/A");
    }

    #[test]
    fn braces_wrap_multi_variable_sections() {
        let got = r(
            "{$band}/{$year - $album}/{$track - $title}.{$ext}",
            &[
                (MetaKey::Band, "B"),
                (MetaKey::Year, "2001"),
                (MetaKey::Album, "A"),
                (MetaKey::Track, "7"),
                (MetaKey::Title, "T"),
            ],
            "mp3",
        )
        .unwrap();
        assert_eq!(got, "/out/B/2001 - A/07 - T.mp3");
    }
}
