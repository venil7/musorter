//! Phase 5 — line report + summary + exit code (SPEC "report at the end").
//!
//! Line pattern: `{what happened}: {path}` (transformations append
//! `, source: {old path}`). All lines are sorted by source path so output is
//! deterministic regardless of thread scheduling.

use crate::exec::{ExecResult, Outcome};
use crate::model::{Plan, PlannedOp};

struct Line {
    sort: Vec<u8>,
    success: bool,
    text: String,
}

pub struct Summary {
    pub processed: usize,
    pub skipped_identical: usize,
    pub deduplicated: usize,
    pub junk_removed: usize,
    pub unprocessed: usize,
}

pub fn render(p: &Plan, exec: Option<&ExecResult>, omit_success: bool) -> (Vec<String>, Summary) {
    let mut lines: Vec<Line> = Vec::new();

    let outcome_for = |idx: usize| -> Option<&Result<Outcome, String>> {
        exec.and_then(|e| e.results.iter().find(|(i, _)| *i == idx).map(|(_, r)| r))
    };

    for (i, e) in p.entries.iter().enumerate() {
        let sort = e.file.rel.as_os_str().as_encoded_bytes().to_vec();
        let src = e.file.src.display();

        let (success, text) = match &e.op {
            PlannedOp::Unprocessed { reason } => (false, format!("{reason}: {src}")),
            PlannedOp::Junk => match outcome_for(i) {
                Some(Ok(Outcome::JunkRemoved)) => (true, format!("removed (junk): {src}")),
                _ => (true, format!("ignored (junk): {src}")),
            },
            PlannedOp::Move { dst } => {
                if p.dry_run {
                    (true, format!("moved: {}, source: {src}", dst.display()))
                } else {
                    match outcome_for(i) {
                        Some(Ok(Outcome::Moved { dst })) => {
                            (true, format!("moved: {}, source: {src}", dst.display()))
                        }
                        Some(Err(err)) => (false, format!("{err}: {src}")),
                        _ => (false, format!("internal: no outcome for {src}")),
                    }
                }
            }
            PlannedOp::Copy { dst } => {
                if p.dry_run {
                    (true, format!("copied: {}, source: {src}", dst.display()))
                } else {
                    match outcome_for(i) {
                        Some(Ok(Outcome::Copied { dst })) => {
                            (true, format!("copied: {}, source: {src}", dst.display()))
                        }
                        Some(Err(err)) => (false, format!("{err}: {src}")),
                        _ => (false, format!("internal: no outcome for {src}")),
                    }
                }
            }
            PlannedOp::SkipIdentical { dst, .. } => {
                let verb = if p.copy_mode {
                    "skipped (identical)"
                } else {
                    "deduplicated"
                };
                if p.dry_run {
                    (true, format!("{verb}: {}, source: {src}", dst.display()))
                } else {
                    match outcome_for(i) {
                        Some(Ok(Outcome::Deduplicated { dst }))
                        | Some(Ok(Outcome::SkippedIdentical { dst })) => {
                            (true, format!("{verb}: {}, source: {src}", dst.display()))
                        }
                        Some(Err(err)) => (false, format!("{err}: {src}")),
                        _ => (false, format!("internal: no outcome for {src}")),
                    }
                }
            }
            PlannedOp::Dedup { dst, .. } => {
                if p.dry_run {
                    (
                        true,
                        format!("deduplicated: {}, source: {src}", dst.display()),
                    )
                } else {
                    match outcome_for(i) {
                        Some(Ok(Outcome::Deduplicated { dst })) => (
                            true,
                            format!("deduplicated: {}, source: {src}", dst.display()),
                        ),
                        // update-meta fallback: the duplicate was tag-updated
                        // and moved to a free suffix slot instead.
                        Some(Ok(Outcome::Moved { dst })) => {
                            (true, format!("moved: {}, source: {src}", dst.display()))
                        }
                        Some(Err(err)) => (false, format!("{err}: {src}")),
                        _ => (false, format!("internal: no outcome for {src}")),
                    }
                }
            }
        };

        lines.push(Line {
            sort,
            success,
            text,
        });
    }

    lines.sort_by(|a, b| a.sort.cmp(&b.sort));

    let mut summary = Summary {
        processed: 0,
        skipped_identical: 0,
        deduplicated: 0,
        junk_removed: 0,
        unprocessed: 0,
    };
    for l in &lines {
        if l.success {
            if l.text.starts_with("moved:") || l.text.starts_with("copied:") {
                summary.processed += 1;
            } else if l.text.starts_with("skipped (identical):") {
                summary.skipped_identical += 1;
            } else if l.text.starts_with("deduplicated:") {
                summary.deduplicated += 1;
            } else if l.text.starts_with("removed (junk):") {
                summary.junk_removed += 1;
            }
        } else {
            summary.unprocessed += 1;
        }
    }

    let out: Vec<String> = lines
        .iter()
        .filter(|l| !omit_success || !l.success)
        .map(|l| l.text.clone())
        .collect();
    (out, summary)
}

pub fn summary_block(s: &Summary) -> Vec<String> {
    vec![
        "---".to_string(),
        format!("processed: {}", s.processed),
        format!("skipped (identical): {}", s.skipped_identical),
        format!("deduplicated: {}", s.deduplicated),
        format!("junk removed: {}", s.junk_removed),
        format!("unprocessed: {}", s.unprocessed),
    ]
}
