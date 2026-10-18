//! Phase 2 — build the full (in → out) map before touching anything:
//! render destinations, detect conflicts, hash **only conflict participants**,
//! then allocate final destinations deterministically (Stage A/B/C per
//! execution-plan §P5).
//!
//! In the common case (no destination conflicts) **zero bytes are hashed**.

use crate::cli::Config;
use crate::error::AppError;
use crate::hash::HashCache;
use crate::meta;
use crate::model::*;
use crate::scan;
use crate::template::Template;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A file occupying a destination, discovered on disk before allocation.
struct DiskEntry {
    path: PathBuf,
    size: u64,
}

/// A destination taken earlier in this run.
struct InRunEntry {
    /// Source path of the occupant (hashed on demand if needed).
    src: PathBuf,
    size: u64,
}

pub async fn build(cfg: &Config) -> Result<Plan, AppError> {
    let (files, dirs) = scan::scan(&cfg.input)?;

    // ---- progress bar phase 1: metadata loading ----
    let n_resolve = files
        .iter()
        .filter(|f| !f.symlink && matches!(f.class, FileClass::Audio | FileClass::Image))
        .count();
    let pb = crate::ui::progress(n_resolve as u64, "loading metadata");

    // ---- metadata resolution (parallel, no disk writes) ----
    let resolved = meta::resolve_all(&files, &cfg.overrides, cfg.jobs, &pb).await;

    // ---- Stage A0: band/album consolidation (SPEC) ----
    let (band_map, album_maps) = build_consolidation(&resolved);
    // Path-only views: tags keep their own values for `--update-meta`.
    let path_meta: Vec<Option<ResolvedMeta>> = resolved
        .iter()
        .map(|m| m.as_ref().map(|m| consolidated(m, &band_map, &album_maps)))
        .collect();

    // ---- Stage A1: render destinations (audio first, art aligned after) ----
    enum Rendered {
        Dst(PathBuf),
        Unprocessed(String),
        Junk,
    }
    let mut rendered: Vec<Rendered> = files
        .iter()
        .map(|f| match f.class {
            FileClass::Junk => Rendered::Junk,
            FileClass::Unknown => Rendered::Unprocessed(if f.symlink {
                "symlink".to_string()
            } else {
                "unsupported file type".to_string()
            }),
            // placeholders, filled by the two passes below
            FileClass::Audio | FileClass::Image => Rendered::Unprocessed(String::new()),
        })
        .collect();

    // pass 1: audio → dst, plus source-dir → destination-dir histogram
    let mut audio_dirs: HashMap<PathBuf, HashMap<PathBuf, usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        if f.class != FileClass::Audio {
            continue;
        }
        match render_one(&cfg.template, &cfg.out, f, path_meta[i].as_ref()) {
            Ok(dst) => {
                if let (Some(src_dir), Some(dst_dir)) = (f.src.parent(), dst.parent()) {
                    *audio_dirs
                        .entry(src_dir.to_path_buf())
                        .or_default()
                        .entry(dst_dir.to_path_buf())
                        .or_default() += 1;
                }
                rendered[i] = Rendered::Dst(dst);
            }
            Err(reason) => rendered[i] = Rendered::Unprocessed(reason),
        }
    }

    // pass 2: album art — follow its sibling audio files when possible,
    // otherwise fall back to path inference (SPEC "album art").
    for (i, f) in files.iter().enumerate() {
        if f.class != FileClass::Image {
            continue;
        }
        let aligned = f
            .src
            .parent()
            .and_then(|d| audio_dirs.get(d))
            .and_then(|hist| {
                hist.iter()
                    .max_by_key(|(dir, cnt)| {
                        (**cnt, std::cmp::Reverse(dir.as_os_str().as_encoded_bytes()))
                    })
                    .map(|(dir, _)| dir.clone())
            });
        rendered[i] = match aligned {
            Some(dir) => Rendered::Dst(dir.join(art_name(f))),
            None => match render_one(&cfg.template, &cfg.out, f, path_meta[i].as_ref()) {
                Ok(dst) => Rendered::Dst(dst),
                Err(reason) => Rendered::Unprocessed(reason),
            },
        };
    }

    // ---- progress bar phase 2: planning ----
    pb.set_position(0);
    pb.set_length(files.len() as u64);
    pb.set_message("planning destinations");

    // ---- Stage A2: disk occupancy of every relevant parent directory ----
    let mut parents: HashSet<PathBuf> = HashSet::new();
    for r in &rendered {
        if let Rendered::Dst(dst) = r
            && let Some(p) = dst.parent()
        {
            parents.insert(p.to_path_buf());
        }
    }
    let mut disk: HashMap<String, DiskEntry> = HashMap::new();
    for p in parents {
        let Ok(rd) = std::fs::read_dir(&p) else {
            continue; // directory doesn't exist yet → everything free
        };
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if !md.is_file() {
                continue;
            }
            let key = key_of(&e.path());
            disk.insert(
                key,
                DiskEntry {
                    path: e.path(),
                    size: md.len(),
                },
            );
        }
    }

    // ---- Stage A3: decide which files need md5 (conflicts only) ----
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, r) in rendered.iter().enumerate() {
        if let Rendered::Dst(dst) = r {
            groups.entry(key_of(dst)).or_default().push(i);
        }
    }
    let mut to_hash: HashSet<PathBuf> = HashSet::new();
    for members in groups.values() {
        if members.len() > 1 {
            // Input conflict on the destination path → hash every member.
            for &i in members {
                to_hash.insert(files[i].src.clone());
            }
        }
        // Existing destination files that could prove identical/different:
        // walk the suffix chain, stat-only, marking size-equal comparisons.
        let base = match &rendered[members[0]] {
            Rendered::Dst(d) => d.clone(),
            _ => continue,
        };
        for &i in members {
            let mut idx: u32 = 0;
            while idx < 4096 {
                let cand = candidate_path(&base, idx);
                match disk.get(&key_of(&cand)) {
                    Some(d) => {
                        if d.size == files[i].size {
                            to_hash.insert(files[i].src.clone());
                            to_hash.insert(d.path.clone());
                        }
                        idx += 1;
                    }
                    None => break,
                }
            }
        }
    }

    // ---- Stage B: parallel hashing of exactly the conflict set ----
    let mut cache = HashCache::new();
    cache.fill(to_hash, cfg.jobs).await;

    // ---- Stage C: deterministic allocation ----
    let mut in_run: HashMap<String, InRunEntry> = HashMap::new();
    let mut entries: Vec<PlanEntry> = Vec::with_capacity(files.len());
    let mut alloc = Allocator {
        disk: &disk,
        in_run: &mut in_run,
        cache: &mut cache,
        copy_mode: cfg.copy,
    };

    for (i, f) in files.iter().enumerate() {
        pb.inc(1);
        let meta = resolved[i].clone();
        let (op, my_md5) = match &rendered[i] {
            Rendered::Unprocessed(reason) => (
                PlannedOp::Unprocessed {
                    reason: reason.clone(),
                },
                None,
            ),
            Rendered::Junk => (PlannedOp::Junk, None),
            Rendered::Dst(dst) => alloc.allocate(dst, f).await,
        };
        let md5 = match &op {
            PlannedOp::SkipIdentical { md5, .. } => Some(md5.clone()),
            PlannedOp::Dedup { kept_md5, .. } => Some(kept_md5.clone()),
            // Move/Copy carry an md5 only if a comparison already forced us
            // to compute it — a clean run never hashes anything.
            _ => my_md5,
        };
        entries.push(PlanEntry {
            file: f.clone(),
            op,
            md5,
            meta: if matches!(f.class, FileClass::Audio | FileClass::Image) && !f.symlink {
                meta
            } else {
                None
            },
        });
    }

    // The allocator's borrow of `cache` ends here.
    let hashes_computed = cache.computed();
    pb.finish_and_clear();

    Ok(Plan {
        entries,
        dirs,
        input_root: cfg.input.clone(),
        out: cfg.out.clone(),
        dry_run: cfg.dry_run,
        copy_mode: cfg.copy,
        update_meta: cfg.update_meta,
        jobs: cfg.jobs,
        hashes_computed,
    })
}

struct Allocator<'a> {
    disk: &'a HashMap<String, DiskEntry>,
    in_run: &'a mut HashMap<String, InRunEntry>,
    cache: &'a mut HashCache,
    copy_mode: bool,
}

impl Allocator<'_> {
    /// Returns the operation plus any md5 computed along the way (`None` on
    /// a conflict-free path — nothing is ever hashed needlessly).
    async fn allocate(&mut self, base: &Path, f: &ScannedFile) -> (PlannedOp, Option<String>) {
        let mut cand_idx: u32 = 0;
        let mut my_md5: Option<String> = None; // computed at most once

        loop {
            if cand_idx > 65_535 {
                return (
                    PlannedOp::Unprocessed {
                        reason: "cannot allocate destination".into(),
                    },
                    my_md5,
                );
            }
            let cand = candidate_path(base, cand_idx);
            let k = key_of(&cand);

            // (a) destination already allocated inside this run
            if let Some(occ) = self.in_run.get(&k) {
                if occ.size == f.size {
                    if my_md5.is_none() {
                        my_md5 = self.cache.get(&f.src).await;
                    }
                    let theirs = self.cache.get(&occ.src).await;
                    if let (Some(a), Some(b)) = (my_md5.clone(), theirs)
                        && a == b
                    {
                        // Same content: redundant input copy.
                        let op = if self.copy_mode {
                            PlannedOp::SkipIdentical { dst: cand, md5: a }
                        } else {
                            PlannedOp::Dedup {
                                dst: cand,
                                kept_md5: a,
                            }
                        };
                        return (op, my_md5);
                    }
                    // hashes unavailable → conservative: never merge/delete
                }
                cand_idx += 1;
                continue;
            }

            // (b) destination exists on disk
            if let Some(d) = self.disk.get(&k) {
                if d.size == f.size {
                    if my_md5.is_none() {
                        my_md5 = self.cache.get(&f.src).await;
                    }
                    let theirs = self.cache.get(&d.path).await;
                    if let (Some(a), Some(b)) = (my_md5.clone(), theirs)
                        && a == b
                    {
                        return (PlannedOp::SkipIdentical { dst: cand, md5: a }, my_md5);
                    }
                    // different content (md5) or unknown → try next suffix
                }
                // size mismatch → provably different, no hashing needed
                cand_idx += 1;
                continue;
            }

            // (c) free slot
            self.in_run.insert(
                k,
                InRunEntry {
                    src: f.src.clone(),
                    size: f.size,
                },
            );
            let op = if self.copy_mode {
                PlannedOp::Copy { dst: cand }
            } else {
                PlannedOp::Move { dst: cand }
            };
            return (op, my_md5);
        }
    }
}

/// Render the destination for one file (audio: full template; image: only the
/// directory sections plus the original file name).
/// Sanitized original file name for album art (kept verbatim otherwise:
/// `folder.jpg` stays `folder.jpg`, no title-casing).
fn art_name(f: &ScannedFile) -> String {
    let name =
        crate::sanitize::segment(f.src.file_name().unwrap_or_default().to_str().unwrap_or(""));
    if name.is_empty() {
        "_".to_string()
    } else {
        name
    }
}

/// Path-only metadata: band chain + album replaced by their consolidation
/// representatives (SPEC "album & band consolidation"). The original
/// metadata is untouched — `--update-meta` persists each file's own values.
fn consolidated(
    m: &ResolvedMeta,
    band_map: &HashMap<String, String>,
    album_maps: &HashMap<String, HashMap<String, String>>,
) -> ResolvedMeta {
    let mut pm = m.clone();
    if let Some(cur) = m.band()
        && let Some(rep) = band_map.get(cur).filter(|r| r.as_str() != cur)
    {
        for k in [MetaKey::Artist, MetaKey::AlbumArtist, MetaKey::Band] {
            if m.get(k) == Some(cur) {
                pm.fields.insert(k, rep.clone());
            }
        }
    }
    if let Some(album) = m.get(MetaKey::Album) {
        let bucket = pm.band().unwrap_or("").to_string();
        if let Some(rep) = album_maps
            .get(&bucket)
            .and_then(|map| map.get(album))
            .filter(|r| r.as_str() != album)
        {
            pm.fields.insert(MetaKey::Album, rep.clone());
        }
    }
    pm
}

/// Build the band cluster (global) and album clusters (per consolidated band).
fn build_consolidation(
    resolved: &[Option<ResolvedMeta>],
) -> (
    HashMap<String, String>,
    HashMap<String, HashMap<String, String>>,
) {
    let band_vals = resolved
        .iter()
        .flatten()
        .filter_map(|m| m.band().map(str::to_string));
    let band_map = crate::consolidate::cluster(band_vals);

    let mut buckets: HashMap<String, Vec<String>> = HashMap::new();
    for m in resolved.iter().flatten() {
        if let Some(album) = m.get(MetaKey::Album) {
            let band = m
                .band()
                .map(|b| band_map.get(b).map(String::as_str).unwrap_or(b).to_string())
                .unwrap_or_default();
            buckets.entry(band).or_default().push(album.to_string());
        }
    }
    let album_maps = buckets
        .into_iter()
        .map(|(band, albums)| (band, crate::consolidate::cluster(albums)))
        .collect();
    (band_map, album_maps)
}

fn render_one(
    t: &Template,
    out: &Path,
    f: &ScannedFile,
    m: Option<&ResolvedMeta>,
) -> Result<PathBuf, String> {
    if f.class == FileClass::Image {
        return t
            .render_dir_sections(out, f, m)
            .map_err(|_| "cannot determine album".to_string())
            .map(|dir| dir.join(art_name(f)));
    }
    t.render(out, f, m).map_err(|k| reason_for(k, m, f))
}

fn reason_for(key: MetaKey, m: Option<&ResolvedMeta>, f: &ScannedFile) -> String {
    let missing = format!("missing {}", key.name());
    // A tag read failure is the likely root cause — surface it.
    if m.and_then(|m| m.tag_error.as_ref()).is_some() && key != MetaKey::Band {
        return format!("unreadable tags ({missing})");
    }
    let _ = f;
    missing
}

/// Lower-cased absolute path string — the case-insensitive occupancy key
/// (`Cover.jpg`/`cover.jpg` collide on macOS/Windows and must collide here).
pub fn key_of(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

/// `Song.flac` → `Song.flac` (0), `Song-1.flac` (1), `Song-2.flac` (2)…
pub fn candidate_path(base: &Path, idx: u32) -> PathBuf {
    if idx == 0 {
        return base.to_path_buf();
    }
    let ext = base.extension().map(|e| e.to_os_string());
    let stem = base
        .file_stem()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("file"));
    let mut name = stem;
    name.push(format!("-{idx}"));
    if let Some(e) = ext {
        name.push(".");
        name.push(e);
    }
    base.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_suffixed_paths() {
        let p = PathBuf::from("/out/Band/01 - Song.flac");
        assert_eq!(candidate_path(&p, 0), p);
        assert_eq!(
            candidate_path(&p, 1),
            PathBuf::from("/out/Band/01 - Song-1.flac")
        );
        assert_eq!(
            candidate_path(&p, 12),
            PathBuf::from("/out/Band/01 - Song-12.flac")
        );
    }

    #[test]
    fn candidate_no_extension() {
        let p = PathBuf::from("/out/file");
        assert_eq!(candidate_path(&p, 2), PathBuf::from("/out/file-2"));
    }

    #[test]
    fn keys_are_case_insensitive() {
        assert_eq!(
            key_of(Path::new("/a/Cover.JPG")),
            key_of(Path::new("/a/cover.jpg"))
        );
    }
}
