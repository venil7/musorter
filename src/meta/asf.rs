//! Minimal in-house ASF (WMA) tag reader/writer.
//!
//! lofty does not support ASF, and no maintained Rust crate does either
//! (verified Oct 2026), so musorter ships its own implementation covering the
//! objects that carry descriptive metadata:
//!
//! - **Content Description Object** — Title / Author / Copyright /
//!   Description / Rating (UTF-16LE, length-prefixed)
//! - **Extended Content Description Object** — named descriptor list
//!   (data length = DWORD), the classic carrier of `WM/…` keys
//! - **Metadata Library Object** (`D2D0A440-…`) — same descriptor list but
//!   with a WORD data length; this is what ffmpeg writes, verified byte-level
//!
//! Writing updates *every* descriptor container present (so stale copies of a
//! value can never shadow a fresh one) and only creates a new Extended
//! Content Description object when none exists. The header's size and object
//! count are cascaded; the data payload stays byte-identical; the file is
//! replaced atomically (temp file + rename).

use crate::meta::normalize::leading_year;
use crate::model::{MetaKey, ResolvedMeta};
use std::collections::HashMap;
use std::path::Path;

const GUID_HEADER: [u8; 16] = [
    0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C,
];
const GUID_CONTENT_DESC: [u8; 16] = [
    0x33, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C,
];
const GUID_EXT_CONTENT_DESC: [u8; 16] = [
    0x40, 0xB2, 0x75, 0x91, 0x64, 0x67, 0x4F, 0x9C, 0x97, 0xA6, 0x0D, 0xF5, 0x60, 0x99, 0x7C, 0x5C,
];
const GUID_METADATA_LIBRARY: [u8; 16] = [
    0x40, 0xA4, 0xD0, 0xD2, 0x07, 0xE3, 0xD2, 0x11, 0x97, 0xF0, 0x00, 0xA0, 0xC9, 0x5E, 0xA8, 0x50,
];

const HEADER_PREFIX: usize = 30; // 16 GUID + 8 size + 4 count + 2 reserved
const OBJ_HEADER: usize = 24; // 16 GUID + 8 size

fn err(msg: impl Into<String>) -> String {
    msg.into()
}

struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    fn need(&self, off: usize, len: usize) -> Result<&'a [u8], String> {
        self.b
            .get(off..off.saturating_add(len))
            .ok_or_else(|| err("truncated ASF file"))
    }

    fn u16(&self, off: usize) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.need(off, 2)?.try_into().unwrap()))
    }

    fn u32(&self, off: usize) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.need(off, 4)?.try_into().unwrap()))
    }

    fn u64(&self, off: usize) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.need(off, 8)?.try_into().unwrap()))
    }
}

struct Child {
    guid: [u8; 16],
    /// Absolute offset in the file.
    off: usize,
    /// Full object length including its 24-byte header.
    size: u64,
}

/// Parse the header object; returns children offsets.
fn parse_header(b: &[u8]) -> Result<(u64, Vec<Child>), String> {
    let r = Reader { b };
    if r.need(0, 16)? != GUID_HEADER {
        return Err(err("not an ASF/WMA file (bad header GUID)"));
    }
    let header_size = r.u64(16)?;
    if header_size < HEADER_PREFIX as u64 || header_size as usize > b.len() {
        return Err(err("invalid ASF header size"));
    }
    let end = header_size as usize;
    let mut children = Vec::new();
    let mut pos = HEADER_PREFIX;
    while pos + OBJ_HEADER <= end {
        let mut guid = [0u8; 16];
        guid.copy_from_slice(r.need(pos, 16)?);
        let size = r.u64(pos + 16)?;
        if size < OBJ_HEADER as u64 || pos as u64 + size > header_size {
            return Err(err("corrupt ASF child object"));
        }
        children.push(Child {
            guid,
            off: pos,
            size,
        });
        pos += size as usize;
    }
    if pos != end {
        return Err(err("corrupt ASF header (object overrun)"));
    }
    Ok((header_size, children))
}

fn utf16_decode(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_matches('\0')
        .to_string()
}

fn utf16_encode(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

// ---------------------------------------------------------------------------
// Content Description Object
// ---------------------------------------------------------------------------

struct ContentDesc {
    title: String,
    author: String,
    copyright: String,
    description: String,
    rating: String,
}

fn parse_content_desc(b: &[u8], c: &Child) -> Result<ContentDesc, String> {
    let base = c.off + OBJ_HEADER;
    let r = Reader { b };
    let lens: Vec<usize> = (0..5)
        .map(|i| r.u16(base + i * 2).map(|v| v as usize))
        .collect::<Result<_, _>>()?;
    let mut off = base + 10;
    let mut fields = Vec::with_capacity(5);
    for len in lens {
        fields.push(utf16_decode(r.need(off, len)?));
        off += len;
    }
    Ok(ContentDesc {
        title: fields[0].clone(),
        author: fields[1].clone(),
        copyright: fields[2].clone(),
        description: fields[3].clone(),
        rating: fields[4].clone(),
    })
}

fn serialize_content_desc(cd: &ContentDesc) -> Vec<u8> {
    let vals = [
        cd.title.as_str(),
        cd.author.as_str(),
        cd.copyright.as_str(),
        cd.description.as_str(),
        cd.rating.as_str(),
    ];
    let encoded: Vec<Vec<u8>> = vals.iter().map(|v| utf16_encode(v)).collect();
    let mut payload = Vec::new();
    for e in &encoded {
        payload.extend_from_slice(&(e.len() as u16).to_le_bytes());
    }
    for e in &encoded {
        payload.extend_from_slice(e);
    }
    wrap_object(GUID_CONTENT_DESC, payload)
}

fn wrap_object(guid: [u8; 16], payload: Vec<u8>) -> Vec<u8> {
    let size = (OBJ_HEADER + payload.len()) as u64;
    let mut out = Vec::with_capacity(size as usize);
    out.extend_from_slice(&guid);
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

// ---------------------------------------------------------------------------
// Descriptor containers (Extended Content Description / Metadata Library)
// ---------------------------------------------------------------------------

/// Width of the data-length field inside a descriptor list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Layout {
    Dword, // classic Extended Content Description
    Word,  // Metadata Library Object (ffmpeg's choice)
}

struct Descriptor {
    name: String,
    dtype: u16,
    data: Vec<u8>,
}

fn normalized_name(name: &str) -> String {
    let lower = name.trim().trim_matches('\0').to_ascii_lowercase();
    let stripped = lower
        .strip_prefix("wm/")
        .or_else(|| lower.strip_prefix("wm\\"))
        .unwrap_or(&lower);
    stripped.to_string()
}

/// Strict descriptor-list parse: must consume exactly `[start, end)`.
fn parse_descs(
    b: &[u8],
    start: usize,
    end: usize,
    layout: Layout,
) -> Result<Vec<Descriptor>, String> {
    let r = Reader { b };
    if start >= end {
        return Err(err("empty descriptor payload"));
    }
    let count = r.u16(start)? as usize;
    let mut off = start + 2;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let name_len = r.u16(off)? as usize;
        off += 2;
        let name = utf16_decode(r.need(off, name_len)?);
        off += name_len;
        let dtype = r.u16(off)?;
        off += 2;
        let data_len = match layout {
            Layout::Dword => r.u32(off)? as usize,
            Layout::Word => r.u16(off)? as usize,
        };
        off += match layout {
            Layout::Dword => 4,
            Layout::Word => 2,
        };
        let data = r.need(off, data_len)?.to_vec();
        off += data_len;
        if off > end {
            return Err(err("descriptor overruns object"));
        }
        out.push(Descriptor { name, dtype, data });
    }
    if off != end {
        return Err(err("descriptor list does not consume object exactly"));
    }
    Ok(out)
}

/// Try DWORD first (the classic layout), then WORD — return parsed list and
/// the layout that fit.
fn detect_descs(b: &[u8], start: usize, end: usize) -> Result<(Vec<Descriptor>, Layout), String> {
    if let Ok(v) = parse_descs(b, start, end, Layout::Dword) {
        return Ok((v, Layout::Dword));
    }
    parse_descs(b, start, end, Layout::Word).map(|v| (v, Layout::Word))
}

fn serialize_descs(descs: &[Descriptor], layout: Layout) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&(descs.len() as u16).to_le_bytes());
    for d in descs {
        let name = utf16_encode(&d.name);
        payload.extend_from_slice(&(name.len() as u16).to_le_bytes());
        payload.extend_from_slice(&name);
        payload.extend_from_slice(&d.dtype.to_le_bytes());
        match layout {
            Layout::Dword => payload.extend_from_slice(&(d.data.len() as u32).to_le_bytes()),
            Layout::Word => payload.extend_from_slice(&(d.data.len() as u16).to_le_bytes()),
        }
        payload.extend_from_slice(&d.data);
    }
    payload
}

fn unicode_descriptor(name: &str, value: &str) -> Descriptor {
    Descriptor {
        name: name.to_string(),
        dtype: 0, // UNICODE
        data: utf16_encode(value),
    }
}

fn descriptor_text(d: &Descriptor) -> Option<String> {
    let s = match d.dtype {
        0 => utf16_decode(&d.data),
        5 => d
            .data
            .get(0..2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]).to_string())
            .unwrap_or_default(),
        3 => u32::from_le_bytes(d.data.get(0..4)?.try_into().ok()?).to_string(),
        _ => return None,
    };
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn read_tags(path: &Path) -> Result<HashMap<MetaKey, String>, String> {
    let b = std::fs::read(path).map_err(|e| e.to_string())?;
    let (_, children) = parse_header(&b)?;

    let mut out = HashMap::new();
    let mut put = |k: MetaKey, v: Option<String>| {
        if let Some(v) = v {
            let v = v.trim().to_string();
            if !v.is_empty() {
                out.entry(k).or_insert(v); // first source wins
            }
        }
    };

    // Content Description Object → title / artist / comment (read first so
    // the primary fields win over extended duplicates).
    if let Some(cd) = children.iter().find(|c| c.guid == GUID_CONTENT_DESC) {
        let cd = parse_content_desc(&b, cd)?;
        put(MetaKey::Title, nonempty(cd.title));
        put(MetaKey::Artist, nonempty(cd.author));
        put(MetaKey::Comment, nonempty(cd.description));
    }

    // Descriptor containers, in child order (first wins).
    let mut date_val: Option<String> = None;
    for c in &children {
        if c.guid != GUID_EXT_CONTENT_DESC && c.guid != GUID_METADATA_LIBRARY {
            continue;
        }
        let (descs, _) = match detect_descs(&b, c.off + OBJ_HEADER, c.off + c.size as usize) {
            Ok(v) => v,
            Err(_) => continue, // tolerate an unparsable container
        };
        for d in &descs {
            let key = normalized_name(&d.name);
            let Some(val) = descriptor_text(d) else {
                continue;
            };
            match key.as_str() {
                "albumtitle" => put(MetaKey::Album, Some(val)),
                "albumartist" => put(MetaKey::AlbumArtist, Some(val)),
                "tracknumber" | "track" => put(MetaKey::Track, Some(val)),
                "discnumber" | "disc" => put(MetaKey::Disc, Some(val)),
                "genre" => put(MetaKey::Genre, Some(val)),
                "composer" => put(MetaKey::Composer, Some(val)),
                "artists" | "contributingartists" => put(MetaKey::Artists, Some(val)),
                "band" => put(MetaKey::Band, Some(val)),
                "comment" => put(MetaKey::Comment, Some(val)),
                "artist" | "author" => put(MetaKey::Artist, Some(val)),
                "title" => put(MetaKey::Title, Some(val)),
                "year" => put(MetaKey::Year, Some(val)),
                "date" | "releasedate" => {
                    put(MetaKey::Date, Some(val.clone()));
                    date_val = Some(val);
                }
                _ => {}
            }
        }
    }

    if !out.contains_key(&MetaKey::Year)
        && let Some(d) = date_val.or_else(|| out.get(&MetaKey::Date).cloned())
        && let Some(y) = leading_year(&d)
    {
        out.insert(MetaKey::Year, y);
    }

    Ok(out)
}

fn nonempty(s: String) -> Option<String> {
    let t = s.trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// Canonical write names for our extended fields.
const EXTENDED_WRITES: [(&str, MetaKey); 9] = [
    ("WM/AlbumTitle", MetaKey::Album),
    ("WM/AlbumArtist", MetaKey::AlbumArtist),
    ("WM/TrackNumber", MetaKey::Track),
    ("WM/DiscNumber", MetaKey::Disc),
    ("WM/Genre", MetaKey::Genre),
    ("WM/Composer", MetaKey::Composer),
    ("WM/Year", MetaKey::Year),
    ("WM/ReleaseDate", MetaKey::Date),
    ("WM/Band", MetaKey::Band),
];

fn upsert(descs: &mut Vec<Descriptor>, name: &str, value: &str) -> bool {
    let target = normalized_name(name);
    // Contributing artists appear under two spellings in the wild.
    let alias = target == "contributingartists";
    let found = descs.iter().position(|d| {
        let n = normalized_name(&d.name);
        n == target || (alias && (n == "artists" || n == "contributingartists"))
    });
    match found {
        Some(i) => {
            if descriptor_text(&descs[i]).as_deref() == Some(value) {
                return false;
            }
            // Preserve the original descriptor name; refresh the value.
            descs[i] = unicode_descriptor(&descs[i].name, value);
            true
        }
        None => {
            descs.push(unicode_descriptor(name, value));
            true
        }
    }
}

pub fn write_tags(path: &Path, meta: &ResolvedMeta) -> Result<(), String> {
    use std::io::Write;

    let b = std::fs::read(path).map_err(|e| e.to_string())?;
    let (old_header_size, children) = parse_header(&b)?;

    let get =
        |k: MetaKey| -> Option<String> { meta.fields.get(&k).cloned().filter(|v| !v.is_empty()) };

    // ---- Content Description (Title / Author / Description) ----
    let cd_child = children.iter().position(|c| c.guid == GUID_CONTENT_DESC);
    let mut cd = match cd_child {
        Some(i) => parse_content_desc(&b, &children[i])?,
        None => ContentDesc {
            title: String::new(),
            author: String::new(),
            copyright: String::new(),
            description: String::new(),
            rating: String::new(),
        },
    };
    let mut cd_changed = false;
    if let Some(v) = get(MetaKey::Title) {
        cd_changed |= cd.title != v;
        cd.title = v;
    }
    if let Some(v) = get(MetaKey::Artist) {
        cd_changed |= cd.author != v;
        cd.author = v;
    }
    if let Some(v) = get(MetaKey::Comment) {
        cd_changed |= cd.description != v;
        cd.description = v;
    }
    let new_cd = serialize_content_desc(&cd);

    // ---- Extended values with Year folding (formats without a year frame) ----
    let mut extended: Vec<(&str, String)> = Vec::new();
    for (name, key) in EXTENDED_WRITES {
        let Some(value) = get(key) else { continue };
        if key == MetaKey::Date && get(MetaKey::Year).as_deref() == Some(value.as_str()) {
            continue;
        }
        extended.push((name, value));
    }
    if let Some(value) = get(MetaKey::Artists) {
        extended.push(("WM/ContributingArtists", value));
    }

    // ---- Update EVERY descriptor container found (in its own layout) ----
    let mut replacements: HashMap<usize, Vec<u8>> = HashMap::new(); // child idx → new bytes
    let mut any_container = false;
    for (i, c) in children.iter().enumerate() {
        if c.guid != GUID_EXT_CONTENT_DESC && c.guid != GUID_METADATA_LIBRARY {
            continue;
        }
        any_container = true;
        let (mut descs, layout) = detect_descs(&b, c.off + OBJ_HEADER, c.off + c.size as usize)
            .map_err(|e| format!("corrupt descriptor container: {e}"))?;
        let mut changed = false;
        for (name, value) in &extended {
            changed |= upsert(&mut descs, name, value);
        }
        if changed {
            replacements.insert(i, wrap_object(c.guid, serialize_descs(&descs, layout)));
        }
    }

    // Create a classic ECD only when the file has no descriptor container at
    // all (and we actually have something to write).
    let created_ecd = if !any_container && !extended.is_empty() {
        let descs: Vec<Descriptor> = extended
            .iter()
            .map(|(n, v)| unicode_descriptor(n, v))
            .collect();
        Some(wrap_object(
            GUID_EXT_CONTENT_DESC,
            serialize_descs(&descs, Layout::Dword),
        ))
    } else {
        None
    };

    if !cd_changed && replacements.is_empty() && created_ecd.is_none() {
        return Ok(()); // nothing to do
    }

    // ---- Rebuild header ----
    let mut out: Vec<u8> = b[..HEADER_PREFIX].to_vec();
    for (i, c) in children.iter().enumerate() {
        if Some(i) == cd_child && cd_changed {
            out.extend_from_slice(&new_cd);
        } else if let Some(nb) = replacements.get(&i) {
            out.extend_from_slice(nb);
        } else {
            let start = c.off;
            let end = c.off + c.size as usize;
            out.extend_from_slice(&b[start..end]);
        }
    }
    let mut count_delta: u32 = 0;
    // The file had no Content Description object at all → append ours.
    if cd_child.is_none() && cd_changed {
        out.extend_from_slice(&new_cd);
        count_delta += 1;
    }
    if let Some(nb) = &created_ecd {
        out.extend_from_slice(nb);
        count_delta += 1;
    }

    // Payload stays byte-identical.
    out.extend_from_slice(&b[old_header_size as usize..]);

    // Cascade header size + object count.
    let new_header_size = (out.len() - (b.len() - old_header_size as usize)) as u64;
    out[16..24].copy_from_slice(&new_header_size.to_le_bytes());
    if count_delta > 0 {
        let old_count = u32::from_le_bytes(out[24..28].try_into().unwrap());
        out[24..28].copy_from_slice(&(old_count + count_delta).to_le_bytes());
    }

    // Atomic replace: temp file in the same directory, then rename.
    let file_name = path
        .file_name()
        .ok_or_else(|| err("bad path"))?
        .to_string_lossy()
        .into_owned();
    let tmp = path.with_file_name(format!(".{file_name}.musorter-tmp"));
    let write_result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&out)?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Source;
    use std::collections::HashMap as Map;

    /// Minimal structurally valid ASF: File Properties + Content Description
    /// + Extended Content Description + fake payload.
    fn build_fixture() -> Vec<u8> {
        let file_props = wrap_object(
            [
                0xA1, 0xDC, 0xAB, 0x8C, 0x47, 0xA9, 0xCF, 0x11, 0x8E, 0xE4, 0x00, 0xC0, 0x0C, 0x20,
                0x53, 0x65,
            ],
            vec![0u8; 80],
        );
        let cd = serialize_content_desc(&ContentDesc {
            title: "Old Title".into(),
            author: "Old Author".into(),
            copyright: "(c)".into(),
            description: "Old Desc".into(),
            rating: String::new(),
        });
        let ecd = wrap_object(
            GUID_EXT_CONTENT_DESC,
            serialize_descs(
                &[
                    unicode_descriptor("WM/AlbumTitle", "Old Album"),
                    unicode_descriptor("WM/TrackNumber", "4"),
                    unicode_descriptor("WM/Genre", "Rock"),
                    unicode_descriptor("Some Unknown Key", "keep me"),
                ],
                Layout::Dword,
            ),
        );

        let children_total = file_props.len() + cd.len() + ecd.len();
        let header_size = HEADER_PREFIX + children_total;

        let mut out = Vec::new();
        out.extend_from_slice(&GUID_HEADER);
        out.extend_from_slice(&(header_size as u64).to_le_bytes());
        out.extend_from_slice(&3u32.to_le_bytes());
        out.push(0x01);
        out.push(0x02);
        out.extend_from_slice(&file_props);
        out.extend_from_slice(&cd);
        out.extend_from_slice(&ecd);
        out.extend_from_slice(b"FAKE ASF PAYLOAD DATA");
        out
    }

    /// Metadata-Library-shaped fixture (WORD data lengths, names with null
    /// terminators) — the exact layout ffmpeg writes.
    fn build_metadata_library_fixture() -> Vec<u8> {
        let cd = serialize_content_desc(&ContentDesc {
            title: "Some Song".into(),
            author: "Some Artist".into(),
            copyright: String::new(),
            description: String::new(),
            rating: String::new(),
        });
        let desc_payload = |name: &str, value: &str| -> Vec<u8> {
            let mut n = utf16_encode(name);
            n.extend_from_slice(&[0, 0]); // null-terminated name (ffmpeg style)
            let mut v = utf16_encode(value);
            v.extend_from_slice(&[0, 0]); // null-terminated value
            let mut p = Vec::new();
            p.extend_from_slice(&1u16.to_le_bytes());
            p.extend_from_slice(&(n.len() as u16).to_le_bytes());
            p.extend_from_slice(&n);
            p.extend_from_slice(&0u16.to_le_bytes()); // UNICODE
            p.extend_from_slice(&(v.len() as u16).to_le_bytes()); // WORD length
            p.extend_from_slice(&v);
            p
        };
        // One Metadata Library object per descriptor (count = 1 each) is NOT
        // how ffmpeg does it — it packs all descriptors into ONE object.
        let mut list = Vec::new();
        let entries = [
            ("title", "Some Song"),
            ("Author", "Some Artist"),
            ("WM/AlbumTitle", "Some Album"),
            ("WM/TrackNumber", "1"),
            ("date", "1980"),
            ("WM/Genre", "rock"),
        ];
        list.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for (n, v) in entries {
            let one = desc_payload(n, v);
            list.extend_from_slice(&one[2..]); // skip the per-entry count
        }
        let meta_obj = wrap_object(GUID_METADATA_LIBRARY, list);
        let _ = desc_payload; // (helper retained for clarity)

        let header_size = HEADER_PREFIX + cd.len() + meta_obj.len();
        let mut out = Vec::new();
        out.extend_from_slice(&GUID_HEADER);
        out.extend_from_slice(&(header_size as u64).to_le_bytes());
        out.extend_from_slice(&2u32.to_le_bytes());
        out.push(0x01);
        out.push(0x02);
        out.extend_from_slice(&cd);
        out.extend_from_slice(&meta_obj);
        out.extend_from_slice(b"PRESERVED PAYLOAD");
        out
    }

    #[test]
    fn reads_fixture() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.wma");
        std::fs::write(&p, build_fixture()).unwrap();
        let m = read_tags(&p).unwrap();
        assert_eq!(
            m.get(&MetaKey::Title).map(String::as_str),
            Some("Old Title")
        );
        assert_eq!(
            m.get(&MetaKey::Artist).map(String::as_str),
            Some("Old Author")
        );
        assert_eq!(
            m.get(&MetaKey::Album).map(String::as_str),
            Some("Old Album")
        );
        assert_eq!(m.get(&MetaKey::Track).map(String::as_str), Some("4"));
        assert_eq!(m.get(&MetaKey::Genre).map(String::as_str), Some("Rock"));
        assert_eq!(
            m.get(&MetaKey::Comment).map(String::as_str),
            Some("Old Desc")
        );
    }

    #[test]
    fn reads_metadata_library_layout() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.wma");
        std::fs::write(&p, build_metadata_library_fixture()).unwrap();
        let m = read_tags(&p).unwrap();
        assert_eq!(
            m.get(&MetaKey::Title).map(String::as_str),
            Some("Some Song")
        );
        assert_eq!(
            m.get(&MetaKey::Artist).map(String::as_str),
            Some("Some Artist")
        );
        assert_eq!(
            m.get(&MetaKey::Album).map(String::as_str),
            Some("Some Album")
        );
        assert_eq!(m.get(&MetaKey::Track).map(String::as_str), Some("1"));
        assert_eq!(m.get(&MetaKey::Year).map(String::as_str), Some("1980"));
        assert_eq!(m.get(&MetaKey::Genre).map(String::as_str), Some("rock"));
    }

    #[test]
    fn roundtrip_write_into_metadata_library_layout() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.wma");
        std::fs::write(&p, build_metadata_library_fixture()).unwrap();

        let mut fields = Map::new();
        fields.insert(MetaKey::Album, "New Album".to_string());
        fields.insert(MetaKey::Genre, "Metal".to_string());
        let meta = ResolvedMeta {
            fields,
            origins: Default::default(),
            tag_error: None,
        };
        write_tags(&p, &meta).unwrap();

        let m = read_tags(&p).unwrap();
        assert_eq!(
            m.get(&MetaKey::Album).map(String::as_str),
            Some("New Album")
        );
        assert_eq!(m.get(&MetaKey::Genre).map(String::as_str), Some("Metal"));
        // untouched values preserved
        assert_eq!(
            m.get(&MetaKey::Title).map(String::as_str),
            Some("Some Song")
        );
        assert_eq!(m.get(&MetaKey::Track).map(String::as_str), Some("1"));
        assert!(
            std::fs::read(&p)
                .unwrap()
                .windows(17)
                .any(|w| w == b"PRESERVED PAYLOAD"),
            "payload must be byte-identical"
        );
    }

    #[test]
    fn roundtrip_write() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.wma");
        std::fs::write(&p, build_fixture()).unwrap();

        let mut fields = Map::new();
        fields.insert(MetaKey::Title, "New Title".to_string());
        fields.insert(MetaKey::Album, "New Album".to_string());
        fields.insert(MetaKey::Track, "07".to_string());
        fields.insert(MetaKey::Year, "1999".to_string());
        fields.insert(MetaKey::Composer, "Someone".to_string());
        let meta = ResolvedMeta {
            fields,
            origins: fields_keys()
                .into_iter()
                .map(|k| (k, Source::Override))
                .collect(),
            tag_error: None,
        };
        write_tags(&p, &meta).unwrap();

        let m = read_tags(&p).unwrap();
        assert_eq!(
            m.get(&MetaKey::Title).map(String::as_str),
            Some("New Title")
        );
        assert_eq!(
            m.get(&MetaKey::Artist).map(String::as_str),
            Some("Old Author")
        );
        assert_eq!(
            m.get(&MetaKey::Album).map(String::as_str),
            Some("New Album")
        );
        assert_eq!(m.get(&MetaKey::Track).map(String::as_str), Some("07"));
        assert_eq!(m.get(&MetaKey::Year).map(String::as_str), Some("1999"));
        assert_eq!(
            m.get(&MetaKey::Composer).map(String::as_str),
            Some("Someone")
        );
        assert_eq!(
            m.get(&MetaKey::Comment).map(String::as_str),
            Some("Old Desc")
        );
        assert_eq!(m.get(&MetaKey::Genre).map(String::as_str), Some("Rock"));

        let raw = std::fs::read(&p).unwrap();
        assert!(raw.windows(21).any(|w| w == b"FAKE ASF PAYLOAD DATA"));
    }

    #[test]
    fn writes_into_file_without_any_descriptor_container() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.wma");

        let cd = serialize_content_desc(&ContentDesc {
            title: "T".into(),
            author: "A".into(),
            copyright: String::new(),
            description: String::new(),
            rating: String::new(),
        });
        let header_size = HEADER_PREFIX + cd.len();
        let mut out = Vec::new();
        out.extend_from_slice(&GUID_HEADER);
        out.extend_from_slice(&(header_size as u64).to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.push(0x01);
        out.push(0x02);
        out.extend_from_slice(&cd);
        out.extend_from_slice(b"payload");
        std::fs::write(&p, out).unwrap();

        let mut fields = Map::new();
        fields.insert(MetaKey::Album, "Fresh".to_string());
        let meta = ResolvedMeta {
            fields,
            origins: Default::default(),
            tag_error: None,
        };
        write_tags(&p, &meta).unwrap();

        let m = read_tags(&p).unwrap();
        assert_eq!(m.get(&MetaKey::Album).map(String::as_str), Some("Fresh"));
        assert_eq!(m.get(&MetaKey::Title).map(String::as_str), Some("T"));
        assert!(
            std::fs::read(&p)
                .unwrap()
                .windows(7)
                .any(|w| w == b"payload")
        );
    }

    fn fields_keys() -> Vec<MetaKey> {
        vec![
            MetaKey::Title,
            MetaKey::Album,
            MetaKey::Track,
            MetaKey::Year,
            MetaKey::Composer,
        ]
    }
}
