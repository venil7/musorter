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

    // ---- metadata resolution (parallel, no disk writes) ----
    let resolved = meta::resolve_all(&files, &cfg.overrides, cfg.jobs).await;

    // ---- Stage A1: render candidate destinations ----
    enum Rendered {
        Dst(PathBuf),
        Unprocessed(String),
        Junk,
    }
    let mut rendered: Vec<Rendered> = Vec::with_capacity(files.len());
    for (f, m) in files.iter().zip(resolved.iter()) {
        let r = match f.class {
            FileClass::Junk => Rendered::Junk,
            FileClass::Unknown => Rendered::Unprocessed(if f.symlink {
                "symlink".to_string()
            } else {
                "unsupported file type".to_string()
            }),
            FileClass::Audio | FileClass::Image => {
                match render_one(&cfg.template, &cfg.out, f, m.as_ref()) {
                    Ok(dst) => Rendered::Dst(dst),
                    Err(reason) => Rendered::Unprocessed(reason),
                }
            }
        };
        rendered.push(r);
    }

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
            .map(|dir| {
                let name = crate::sanitize::segment(
                    f.src.file_name().unwrap_or_default().to_str().unwrap_or(""),
                );
                let name = if name.is_empty() {
                    "_".to_string()
                } else {
                    name
                };
                dir.join(name)
            });
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
