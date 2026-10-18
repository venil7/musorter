//! Phase 3 — execute the plan. Fail-soft per file; source is only deleted
//! after the destination is verified; `--dry-run` never reaches this module.

use crate::hash;
use crate::meta::{asf, tags};
use crate::model::*;
use crate::plan;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

#[derive(Debug, Clone)]
pub enum Outcome {
    Moved {
        dst: PathBuf,
    },
    Copied {
        dst: PathBuf,
    },
    /// Source removed because identical content already sits at `dst`.
    Deduplicated {
        dst: PathBuf,
    },
    /// Copy mode: identical content already at `dst`; source untouched.
    SkippedIdentical {
        dst: PathBuf,
    },
    /// Junk file deleted during move-mode pruning.
    JunkRemoved,
    /// Junk file left in place (copy mode / not prunable).
    JunkKept,
}

pub struct ExecResult {
    /// (entry index, outcome or failure reason)
    pub results: Vec<(usize, Result<Outcome, String>)>,
    pub prune: PruneOutcome,
}

#[derive(Debug, Default)]
pub struct PruneOutcome {
    /// Source paths of junk files deleted during pruning.
    pub removed_junk: Vec<PathBuf>,
    pub removed_dirs: usize,
}

pub async fn execute(p: &Plan) -> ExecResult {
    let mut results: Vec<(usize, Result<Outcome, String>)> = Vec::new();

    // ---- Wave 1: independent operations, parallel & bounded ----
    let sem = Arc::new(Semaphore::new(p.jobs.max(1)));
    let mut set: JoinSet<(usize, Result<Outcome, String>)> = JoinSet::new();

    for (i, e) in p.entries.iter().enumerate() {
        let runnable = matches!(
            e.op,
            PlannedOp::Move { .. } | PlannedOp::Copy { .. } | PlannedOp::SkipIdentical { .. }
        );
        if !runnable {
            continue;
        }
        let permit = match sem.clone().acquire_owned().await {
            Ok(perm) => perm,
            Err(_) => break,
        };
        let entry = e.clone();
        let copy_mode = p.copy_mode;
        let update_meta = p.update_meta;
        set.spawn(async move {
            let _permit = permit;
            let res =
                tokio::task::spawn_blocking(move || wave_one(&entry, copy_mode, update_meta)).await;
            (i, res.unwrap_or_else(|e| Err(format!("task failed: {e}"))))
        });
    }
    while let Some(h) = set.join_next().await {
        if let Ok(r) = h {
            results.push(r);
        }
    }

    // ---- Wave 2: dedup deletions (serial — dest must be settled) ----
    for (i, e) in p.entries.iter().enumerate() {
        if matches!(e.op, PlannedOp::Dedup { .. }) {
            let res = wave_two(e, p).await;
            results.push((i, res));
        }
    }

    // ---- Pruning (move mode only) ----
    let prune = if !p.copy_mode && !p.dry_run {
        crate::prune::prune(p).await
    } else {
        PruneOutcome::default()
    };

    // Junk lines: removed during prune, or kept (copy/dry/prunable-failed).
    let removed: std::collections::HashSet<PathBuf> = prune.removed_junk.iter().cloned().collect();
    for (i, e) in p.entries.iter().enumerate() {
        if matches!(e.op, PlannedOp::Junk) {
            let out = if removed.contains(&e.file.src) {
                Outcome::JunkRemoved
            } else {
                Outcome::JunkKept
            };
            results.push((i, Ok(out)));
        }
    }

    ExecResult { results, prune }
}

// ---------------------------------------------------------------------------
// Wave 1
// ---------------------------------------------------------------------------

fn wave_one(e: &PlanEntry, copy_mode: bool, update_meta: bool) -> Result<Outcome, String> {
    match &e.op {
        PlannedOp::Move { dst } => {
            let wrote = update_meta && e.file.class == FileClass::Audio;
            if wrote {
                write_meta(e)?; // failure ⇒ source untouched, error reported
            }
            ensure_parent(dst)?;
            let expected = e.md5.clone().filter(|_| !wrote);
            move_verified(&e.file.src, dst, e.file.size, expected)?;
            Ok(Outcome::Moved { dst: dst.clone() })
        }
        PlannedOp::Copy { dst } => {
            ensure_parent(dst)?;
            copy_verified(&e.file.src, dst, e.md5.clone())?;
            if update_meta && e.file.class == FileClass::Audio {
                // Tags go onto the destination copy; the source stays pristine.
                if let Err(err) = write_meta_to(dst, e) {
                    let _ = std::fs::remove_file(dst);
                    return Err(err);
                }
            }
            Ok(Outcome::Copied { dst: dst.clone() })
        }
        PlannedOp::SkipIdentical { dst, md5 } => {
            if copy_mode {
                // Nothing to do — identical content already at the destination.
                return Ok(Outcome::SkippedIdentical { dst: dst.clone() });
            }
            // Move mode: verify the destination still matches, then drop the
            // redundant source.
            let now = hash::md5_blocking(dst).map_err(|e| e.to_string())?;
            if &now != md5 {
                return Err(format!("destination {dst:?} changed during run"));
            }
            std::fs::remove_file(&e.file.src).map_err(|e| e.to_string())?;
            Ok(Outcome::Deduplicated { dst: dst.clone() })
        }
        PlannedOp::Dedup { .. } | PlannedOp::Unprocessed { .. } | PlannedOp::Junk => {
            Err("internal: wave-one op dispatched incorrectly".into())
        }
    }
}

// ---------------------------------------------------------------------------
// Wave 2 — redundant input duplicates (move mode)
// ---------------------------------------------------------------------------

async fn wave_two(e: &PlanEntry, p: &Plan) -> Result<Outcome, String> {
    let PlannedOp::Dedup { dst, kept_md5 } = &e.op else {
        return Err("internal: wave-two op is not a dedup".into());
    };
    let dst = dst.clone();
    let kept_md5 = kept_md5.clone();
    let entry = e.clone();
    let update_meta = p.update_meta;

    tokio::task::spawn_blocking(move || {
        // Safety gate: only delete the source when the destination provably
        // holds the same content.
        match hash::md5_blocking(&dst) {
            Ok(h) if h == kept_md5 => {
                std::fs::remove_file(&entry.file.src).map_err(|e| e.to_string())?;
                Ok(Outcome::Deduplicated { dst })
            }
            Ok(_) | Err(_) if update_meta && entry.file.class == FileClass::Audio => {
                // The kept file was tag-updated (content changed), so the
                // duplicate no longer matches it. Give the duplicate the same
                // treatment and place it at the next free suffix slot.
                write_meta(&entry)?;
                let free = first_free(&dst)?;
                ensure_parent(&free)?;
                move_verified(&entry.file.src, &free, entry.file.size, None)?;
                Ok(Outcome::Moved { dst: free })
            }
            Ok(_) => Err(format!("destination {dst:?} failed dedup verification")),
            Err(err) => Err(format!("destination {dst:?}: {err}")),
        }
    })
    .await
    .unwrap_or_else(|e| Err(format!("task failed: {e}")))
}

/// `first_free(Song.flac)` → `Song-1.flac` when `Song.flac` exists, etc.
fn first_free(base: &Path) -> Result<PathBuf, String> {
    for idx in 0..65_536u32 {
        let cand = plan::candidate_path(base, idx);
        if !cand.exists() {
            return Ok(cand);
        }
    }
    Err("cannot allocate suffix for dedup fallback".into())
}

// ---------------------------------------------------------------------------
// Filesystem primitives
// ---------------------------------------------------------------------------

fn ensure_parent(dst: &Path) -> Result<(), String> {
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p).map_err(|e| format!("cannot create {p:?}: {e}"))?;
    }
    Ok(())
}

fn write_meta(e: &PlanEntry) -> Result<(), String> {
    let Some(meta) = &e.meta else { return Ok(()) };
    if meta.fields.is_empty() {
        return Ok(());
    }
    write_meta_to(&e.file.src, e).map_err(|err| format!("tag write failed: {err}"))
}

fn write_meta_to(path: &Path, e: &PlanEntry) -> Result<(), String> {
    let Some(meta) = &e.meta else { return Ok(()) };
    if meta.fields.is_empty() {
        return Ok(());
    }
    if crate::meta::is_asf(&e.file.ext) {
        asf::write_tags(path, meta)
    } else {
        tags::write_tags(path, meta)
    }
}

/// Move with cross-device fallback. The source is removed only after the
/// destination passes verification.
fn move_verified(
    src: &Path,
    dst: &Path,
    _planned_size: u64,
    expected_md5: Option<String>,
) -> Result<(), String> {
    let src_size = std::fs::metadata(src).map_err(|e| e.to_string())?.len();
    match std::fs::rename(src, dst) {
        Ok(()) => {
            verify(dst, src_size, expected_md5.as_deref())?;
            Ok(())
        }
        Err(e) if is_cross_device(&e) => {
            // copy → verify → delete source
            std::fs::copy(src, dst).map_err(|e| format!("copy failed: {e}"))?;
            verify(dst, src_size, expected_md5.as_deref())?;
            std::fs::remove_file(src).map_err(|e| format!("source removal failed: {e}"))?;
            Ok(())
        }
        Err(e) => Err(format!("move failed: {e}")),
    }
}

fn copy_verified(src: &Path, dst: &Path, expected_md5: Option<String>) -> Result<(), String> {
    let src_size = std::fs::metadata(src).map_err(|e| e.to_string())?.len();
    std::fs::copy(src, dst).map_err(|e| format!("copy failed: {e}"))?;
    verify(dst, src_size, expected_md5.as_deref())
}

fn verify(dst: &Path, expected_size: u64, expected_md5: Option<&str>) -> Result<(), String> {
    let md = std::fs::metadata(dst).map_err(|e| format!("verification failed: {e}"))?;
    if md.len() != expected_size {
        return Err(format!(
            "verification failed for {dst:?}: size {} != {expected_size}",
            md.len()
        ));
    }
    if let Some(want) = expected_md5 {
        let got = hash::md5_blocking(dst).map_err(|e| format!("verification failed: {e}"))?;
        if got != want {
            return Err(format!("verification failed for {dst:?}: md5 mismatch"));
        }
    }
    Ok(())
}

fn is_cross_device(e: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(18) // EXDEV
    }
    #[cfg(not(unix))]
    {
        let _ = e;
        false
    }
}
