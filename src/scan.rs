//! Phase 1: walk the input tree, classify every entry, record directories.

use crate::error::AppError;
use crate::model::{FileClass, ScannedFile};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const AUDIO_EXTS: [&str; 17] = [
    "mp3", "flac", "m4a", "mp4", "ogg", "oga", "opus", "aac", "wav", "aiff", "aif", "ape", "wv",
    "wma", "asf", "mpc", "spx",
];
const IMAGE_EXTS: [&str; 7] = ["jpg", "jpeg", "jpe", "png", "gif", "webp", "bmp"];
const JUNK_EXTS: [&str; 11] = [
    "log", "cue", "m3u", "m3u8", "pls", "sfv", "nfo", "diz", "txt", "url", "md",
];
const JUNK_NAMES: [&str; 3] = [".ds_store", "thumbs.db", "desktop.ini"];

/// Junk = never gets a destination (SPEC "other files").
pub fn is_junk(name_or_ext_name: &str, ext: &str) -> bool {
    let name = name_or_ext_name.to_ascii_lowercase();
    if JUNK_NAMES.contains(&name.as_str()) {
        return true;
    }
    JUNK_EXTS.contains(&ext)
}

pub fn classify(name: &str, ext: &str) -> FileClass {
    if is_junk(name, ext) {
        return FileClass::Junk;
    }
    if AUDIO_EXTS.contains(&ext) {
        return FileClass::Audio;
    }
    if IMAGE_EXTS.contains(&ext) {
        return FileClass::Image;
    }
    FileClass::Unknown
}

pub fn scan(input: &Path) -> Result<(Vec<ScannedFile>, Vec<PathBuf>), AppError> {
    let mut files = Vec::new();
    let mut dirs = Vec::new();

    for entry in WalkDir::new(input)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
    {
        let entry = entry.map_err(|e| {
            AppError::usage(format!("cannot read input tree at {:?}: {e}", e.path()))
        })?;
        let path = entry.path();
        let rel = match path.strip_prefix(input) {
            Ok(r) if !r.as_os_str().is_empty() => r.to_path_buf(),
            _ => continue, // the input root itself
        };
        let ft = entry.file_type();
        if ft.is_dir() {
            dirs.push(path.to_path_buf());
            continue;
        }
        let symlink = ft.is_symlink();
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let class = if symlink {
            FileClass::Unknown
        } else {
            classify(&file_name, &ext)
        };
        files.push(ScannedFile {
            src: path.to_path_buf(),
            rel,
            class,
            ext,
            size,
            symlink,
        });
    }

    // Deterministic order: byte-wise over the relative path.
    files.sort_by(|a, b| {
        a.rel
            .as_os_str()
            .as_encoded_bytes()
            .cmp(b.rel.as_os_str().as_encoded_bytes())
    });
    dirs.sort();
    Ok((files, dirs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(classify("song.mp3", "mp3"), FileClass::Audio);
        assert_eq!(classify("song.M4A", "m4a"), FileClass::Audio);
        assert_eq!(classify("cover.jpg", "jpg"), FileClass::Image);
        assert_eq!(classify("album.cue", "cue"), FileClass::Junk);
        assert_eq!(classify(".DS_Store", ""), FileClass::Junk);
        assert_eq!(classify("Thumbs.db", "db"), FileClass::Junk);
        assert_eq!(classify("desktop.ini", "ini"), FileClass::Junk);
        assert_eq!(classify("program.xyz", "xyz"), FileClass::Unknown);
        assert_eq!(classify("README", ""), FileClass::Unknown);
    }

    #[test]
    fn scan_temp_tree() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("a/b/x.mp3"), b"x").unwrap();
        std::fs::write(dir.path().join("a/log.log"), b"l").unwrap();
        let (files, dirs) = scan(dir.path()).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(dirs.len(), 2);
        assert_eq!(files[0].rel.to_string_lossy(), "a/b/x.mp3");
        assert_eq!(files[0].class, FileClass::Audio);
        assert_eq!(files[1].class, FileClass::Junk);
    }
}
