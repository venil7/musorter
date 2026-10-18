//! Phase 4 — move-mode-only cleanup: bubble upwards, deleting empty input
//! folders (and folders containing only junk files) innermost-first, all the
//! way to the input root. Never runs for `--dry-run` or `--copy`.

use crate::exec::PruneOutcome;
use crate::model::Plan;
use crate::scan::is_junk;
use std::path::Path;

pub async fn prune(p: &Plan) -> PruneOutcome {
    let root = p.input_root.clone();
    let mut dirs = p.dirs.clone();
    // innermost first (deepest paths first), deterministic tie-break
    dirs.sort_by(|a, b| {
        let da = a.components().count();
        let db = b.components().count();
        db.cmp(&da).then_with(|| {
            b.as_os_str()
                .as_encoded_bytes()
                .cmp(a.as_os_str().as_encoded_bytes())
        })
    });

    tokio::task::spawn_blocking(move || {
        let mut out = PruneOutcome::default();
        for d in &dirs {
            prune_dir(d, &mut out);
        }
        // The root follows the same rules (SPEC: bubbles up to the root).
        prune_dir(&root, &mut out);
        out
    })
    .await
    .unwrap_or_default()
}

fn prune_dir(d: &Path, out: &mut PruneOutcome) {
    let Ok(rd) = std::fs::read_dir(d) else {
        return;
    };
    let entries: Vec<_> = rd.flatten().collect();
    if entries.is_empty() {
        if std::fs::remove_dir(d).is_ok() {
            out.removed_dirs += 1;
        }
        return;
    }

    // Every entry must be a regular junk file for the folder to be prunable.
    let all_junk = entries.iter().all(|e| {
        let md = match std::fs::symlink_metadata(e.path()) {
            Ok(m) => m,
            Err(_) => return false,
        };
        if !md.is_file() {
            return false;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let ext = Path::new(&name)
            .extension()
            .map(|x| x.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        is_junk(&name, &ext)
    });
    if !all_junk {
        return;
    }

    let mut ok = true;
    for e in &entries {
        match std::fs::remove_file(e.path()) {
            Ok(()) => out.removed_junk.push(e.path()),
            Err(_) => ok = false,
        }
    }
    if ok && std::fs::remove_dir(d).is_ok() {
        out.removed_dirs += 1;
    }
}
