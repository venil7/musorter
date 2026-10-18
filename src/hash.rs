//! Chunked md5 hashing. md5 is computed **only for files involved in a
//! destination conflict** (SPEC/execution-plan clarification).

use md5::{Digest, Md5};
use std::io::Read;
use std::path::Path;

pub fn md5_blocking(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub async fn md5_file(path: &Path) -> std::io::Result<String> {
    let p = path.to_path_buf();
    tokio::task::spawn_blocking(move || md5_blocking(&p))
        .await
        .map_err(std::io::Error::other)?
}

/// Memoizing md5 cache: hashes are computed on demand (lazily) and merged
/// with results of the parallel pre-fill stage.
#[derive(Debug, Default)]
pub struct HashCache {
    map: std::collections::HashMap<std::path::PathBuf, Option<String>>,
    /// Number of times md5 was actually computed (observability: SPEC
    /// requires hashing only conflict participants — asserted by tests).
    computed: usize,
}

impl HashCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many files were actually hashed so far.
    pub fn computed(&self) -> usize {
        self.computed
    }

    /// md5 of `p`, computing it on first request.
    pub async fn get(&mut self, p: &Path) -> Option<String> {
        if let Some(v) = self.map.get(p) {
            return v.clone();
        }
        let r = md5_file(p).await.ok();
        self.computed += 1;
        self.map.insert(p.to_path_buf(), r.clone());
        r
    }

    /// Parallel pre-fill for the conflict set identified at plan time.
    pub async fn fill(&mut self, paths: impl IntoIterator<Item = std::path::PathBuf>, jobs: usize) {
        let paths: Vec<_> = paths.into_iter().collect();
        if paths.is_empty() {
            return;
        }
        let mut set = tokio::task::JoinSet::new();
        // Chunking keeps task creation cheap for large conflict sets.
        for chunk in paths.chunks(jobs.max(1).max(64)) {
            let chunk = chunk.to_vec();
            set.spawn(async move {
                let mut out = Vec::with_capacity(chunk.len());
                for p in chunk {
                    let r = tokio::task::spawn_blocking({
                        let p = p.clone();
                        move || crate::hash::md5_blocking(&p)
                    })
                    .await
                    .ok()
                    .and_then(Result::ok);
                    out.push((p, r));
                }
                out
            });
        }
        while let Some(h) = set.join_next().await {
            if let Ok(batch) = h {
                for (p, r) in batch {
                    self.computed += 1;
                    self.map.insert(p, r);
                }
            }
        }
    }
}
