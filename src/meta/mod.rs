//! Metadata resolution: override > tag > path, evaluated per field, with all
//! values normalized before storage.

pub mod asf;
pub mod infer;
pub mod normalize;
pub mod tags;

use crate::model::{FileClass, MetaKey, ResolvedMeta, ScannedFile, Source};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Formats the in-house ASF reader handles (lofty cannot).
pub fn is_asf(ext: &str) -> bool {
    matches!(ext, "wma" | "asf" | "wmv")
}

/// Resolve metadata for every scannable file, bounded by `jobs`.
pub async fn resolve_all(
    files: &[ScannedFile],
    overrides: &HashMap<MetaKey, String>,
    jobs: usize,
) -> Vec<Option<ResolvedMeta>> {
    let sem = Arc::new(Semaphore::new(jobs.max(1)));
    let mut set: JoinSet<(usize, Option<ResolvedMeta>)> = JoinSet::new();
    let overrides = Arc::new(overrides.clone());

    for (idx, f) in files.iter().enumerate() {
        if f.symlink || !matches!(f.class, FileClass::Audio | FileClass::Image) {
            continue;
        }
        let permit = match sem.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break, // semaphore closed (never happens)
        };
        let file = f.clone();
        let overrides = overrides.clone();
        set.spawn(async move {
            let _permit = permit;
            let res =
                tokio::task::spawn_blocking(move || resolve_blocking(&file, &overrides)).await;
            (idx, res.unwrap_or(None))
        });
    }

    let mut out: Vec<Option<ResolvedMeta>> = vec![None; files.len()];
    while let Some(h) = set.join_next().await {
        if let Ok((idx, meta)) = h {
            out[idx] = meta;
        }
    }
    out
}

/// Blocking per-file resolution: path (lowest) → tags → overrides (highest).
pub fn resolve_blocking(
    f: &ScannedFile,
    overrides: &HashMap<MetaKey, String>,
) -> Option<ResolvedMeta> {
    let mut m = ResolvedMeta::default();

    // 1. Path/filename inference (lowest precedence).
    for (k, v) in infer::infer(&f.rel) {
        m.put(k, v, Source::Path);
    }

    // 2. Tags (preferred source).
    if f.class == FileClass::Audio && !f.symlink {
        let res = if is_asf(&f.ext) {
            asf::read_tags(&f.src)
        } else {
            tags::read_tags(&f.src)
        };
        match res {
            Ok(vals) => {
                for (k, v) in vals {
                    m.put(k, v, Source::Tag);
                }
            }
            Err(e) => m.tag_error = Some(e),
        }
    }

    // 3. CLI overrides (always win).
    for (&k, v) in overrides {
        m.put(k, v.clone(), Source::Override);
    }

    // Normalize every resolved value in memory (files are only touched by
    // `--update-meta`). Comments are free-form notes and pass through as-is.
    let mut empty_keys = Vec::new();
    for (k, v) in m.fields.iter_mut() {
        if *k == MetaKey::Comment {
            continue;
        }
        *v = normalize::normalize(v);
        if v.trim().is_empty() {
            empty_keys.push(*k);
        }
    }
    for k in empty_keys {
        m.fields.remove(&k);
        m.origins.remove(&k);
    }

    // Derive year from date when only the date survived resolution.
    if !m.fields.contains_key(&MetaKey::Year)
        && let Some(d) = m.fields.get(&MetaKey::Date).cloned()
        && let Some(y) = normalize::leading_year(&d)
    {
        let src = m
            .origins
            .get(&MetaKey::Date)
            .copied()
            .unwrap_or(Source::Path);
        m.put(MetaKey::Year, y, src);
    }

    Some(m)
}
