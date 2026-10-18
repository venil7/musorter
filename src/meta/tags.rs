//! Tag reading/writing via lofty for every format lofty supports.
//! WMA/ASF is handled by the in-house `asf` module instead (lofty cannot
//! read ASF at all).

use crate::meta::normalize::leading_year;
use crate::model::{MetaKey, ResolvedMeta};
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::{ItemKey, Tag};
use std::collections::HashMap;
use std::path::Path;

pub fn read_tags(path: &Path) -> Result<HashMap<MetaKey, String>, String> {
    let tf = lofty::read_from_path(path).map_err(|e| e.to_string())?;
    let Some(tag) = tf.primary_tag().or_else(|| tf.first_tag()) else {
        return Ok(HashMap::new());
    };

    let mut m = HashMap::new();
    let date = tag
        .get_string(ItemKey::RecordingDate)
        .or_else(|| tag.get_string(ItemKey::ReleaseDate));

    put(&mut m, MetaKey::Title, tag.get_string(ItemKey::TrackTitle));
    put(
        &mut m,
        MetaKey::Artist,
        tag.get_string(ItemKey::TrackArtist),
    );
    put(&mut m, MetaKey::Album, tag.get_string(ItemKey::AlbumTitle));
    put(
        &mut m,
        MetaKey::AlbumArtist,
        tag.get_string(ItemKey::AlbumArtist),
    );
    put(&mut m, MetaKey::Track, tag.get_string(ItemKey::TrackNumber));
    put(&mut m, MetaKey::Disc, tag.get_string(ItemKey::DiscNumber));
    put(&mut m, MetaKey::Genre, tag.get_string(ItemKey::Genre));
    put(&mut m, MetaKey::Composer, tag.get_string(ItemKey::Composer));
    put(
        &mut m,
        MetaKey::Artists,
        tag.get_string(ItemKey::TrackArtists),
    );
    put(&mut m, MetaKey::Comment, tag.get_string(ItemKey::Comment));
    put(&mut m, MetaKey::Date, date);

    // Year: explicit year frame, else the leading 4 digits of the date.
    let year = tag
        .get_string(ItemKey::Year)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| date.and_then(leading_year));
    if let Some(y) = year {
        m.entry(MetaKey::Year).or_insert(y);
    }

    // Best-effort freeform "band/group" key (mapped only in some formats).
    if let Some(k) = ItemKey::from_key(tag.tag_type(), "BAND") {
        put(&mut m, MetaKey::Band, tag.get_string(k));
    }

    Ok(m)
}

fn put(m: &mut HashMap<MetaKey, String>, k: MetaKey, v: Option<&str>) {
    if let Some(v) = v {
        let v = v.trim().trim_matches('\0');
        if !v.is_empty() {
            m.entry(k).or_insert_with(|| v.to_string());
        }
    }
}

/// Write the resolved (normalized) values back into the file's tag.
/// Only values that differ from what is stored are touched; frames we do not
/// manage are preserved by lofty on save. Unmappable keys are skipped —
/// except `Year`, which is folded into the date field on formats whose
/// standard date frame is the only date/year carrier (e.g. ID3v2 `TDRC`).
pub fn write_tags(path: &Path, meta: &ResolvedMeta) -> Result<(), String> {
    let mut tf = lofty::read_from_path(path).map_err(|e| e.to_string())?;
    let tag_type = tf.primary_tag_type();

    if tf.tags().is_empty() {
        tf.insert_tag(Tag::new(tag_type));
    }

    // 1. Collect the final (ItemKey → value) write set.
    let mut writes: HashMap<ItemKey, String> = HashMap::new();
    for (&key, value) in &meta.fields {
        let Some(item_key) = item_key_for(key, tag_type) else {
            continue;
        };
        if item_key.map_key(tag_type).is_none() {
            continue;
        }
        writes.insert(item_key, value.clone());
    }

    // 2. Year folding: when the format has no dedicated year frame, the year
    //    must land in the date field, keeping any time part of the date.
    if let Some(year) = meta.fields.get(&MetaKey::Year) {
        let year_key_writable = ItemKey::Year.map_key(tag_type).is_some();
        let date_key_writable = ItemKey::RecordingDate.map_key(tag_type).is_some();
        if !year_key_writable && date_key_writable {
            let new_date = match meta.fields.get(&MetaKey::Date) {
                Some(d) if leading_year(d).as_deref() == Some(year.as_str()) => d.clone(),
                Some(d) => replace_leading_year(d, year).unwrap_or_else(|| year.clone()),
                None => year.clone(),
            };
            writes.insert(ItemKey::RecordingDate, new_date);
        }
    }

    {
        let has_primary = tf.primary_tag().is_some();
        let has_first = tf.first_tag().is_some();
        if !has_primary && !has_first {
            return Err("no writable tag".into());
        }
        let tag = if has_primary {
            tf.primary_tag_mut()
        } else {
            tf.first_tag_mut()
        }
        .expect("tag presence checked above");

        for (item_key, value) in writes {
            let current = tag.get_string(item_key).map(str::trim);
            if current == Some(value.as_str()) {
                continue;
            }
            tag.insert_text(item_key, value);
        }
    }

    tf.save_to_path(path, WriteOptions::default())
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Swap the leading `YYYY` of a date-ish string for another year.
fn replace_leading_year(date: &str, year: &str) -> Option<String> {
    let t = date.trim();
    if t.len() >= 4 && t.is_char_boundary(4) && leading_year(t)?.as_str() == &t[..4] {
        return Some(format!("{year}{}", &t[4..]));
    }
    None
}

fn item_key_for(key: MetaKey, tag_type: lofty::tag::TagType) -> Option<ItemKey> {
    let k = match key {
        MetaKey::Title => ItemKey::TrackTitle,
        MetaKey::Artist => ItemKey::TrackArtist,
        MetaKey::Album => ItemKey::AlbumTitle,
        MetaKey::AlbumArtist => ItemKey::AlbumArtist,
        MetaKey::Track => ItemKey::TrackNumber,
        MetaKey::Disc => ItemKey::DiscNumber,
        MetaKey::Genre => ItemKey::Genre,
        MetaKey::Composer => ItemKey::Composer,
        MetaKey::Artists => ItemKey::TrackArtists,
        MetaKey::Comment => ItemKey::Comment,
        MetaKey::Date => ItemKey::RecordingDate,
        MetaKey::Year => ItemKey::Year,
        MetaKey::Band => ItemKey::from_key(tag_type, "BAND")?,
        MetaKey::Ext => return None,
    };
    Some(k)
}
