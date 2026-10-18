use crate::error::AppError;
use crate::meta::normalize::normalize;
use crate::model::MetaKey;
use crate::template::{DEFAULT_TEMPLATE, Template};
use clap::Parser;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(
    name = "musorter",
    version,
    about = "Sort a music collection into a deterministic, template-driven folder structure"
)]
pub struct Args {
    /// Input folder (arbitrary structure)
    pub input: PathBuf,

    /// Output folder
    #[arg(short = 'o', long = "out")]
    pub out: PathBuf,

    /// Output path template (e.g. "{$band}/{$year - $album}/{$track - $title}.{$ext}")
    #[arg(short = 't', long, default_value = DEFAULT_TEMPLATE)]
    pub template: String,

    /// Show what would happen without writing anything to disk
    #[arg(long)]
    pub dry_run: bool,

    /// Copy files instead of moving them (no pruning in copy mode)
    #[arg(long)]
    pub copy: bool,

    /// Write normalized metadata back into processed files
    #[arg(long)]
    pub update_meta: bool,

    /// Maximum number of concurrent workers (0 = number of CPUs)
    #[arg(long, default_value_t = 0)]
    pub jobs: usize,

    /// Print only failures/unprocessed files in the report
    #[arg(long)]
    pub omit_success: bool,

    // --- global metadata overrides (take precedence over tags and paths) ---
    #[arg(long)]
    pub band: Option<String>,
    #[arg(long)]
    pub artist: Option<String>,
    #[arg(long)]
    pub albumartist: Option<String>,
    #[arg(long)]
    pub album: Option<String>,
    #[arg(long)]
    pub year: Option<String>,
    #[arg(long)]
    pub date: Option<String>,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub track: Option<String>,
    #[arg(long)]
    pub disc: Option<String>,
    #[arg(long)]
    pub genre: Option<String>,
    #[arg(long)]
    pub artists: Option<String>,
    #[arg(long)]
    pub composer: Option<String>,
    #[arg(long)]
    pub comment: Option<String>,
}

pub struct Config {
    pub input: PathBuf,
    pub out: PathBuf,
    pub template: Template,
    pub dry_run: bool,
    pub copy: bool,
    pub update_meta: bool,
    pub jobs: usize,
    pub omit_success: bool,
    pub overrides: HashMap<MetaKey, String>,
}

pub fn validate(args: Args) -> Result<Config, AppError> {
    // Input must exist and be a directory.
    let meta = std::fs::metadata(&args.input)
        .map_err(|e| AppError::usage(format!("cannot access input {:?}: {e}", args.input)))?;
    if !meta.is_dir() {
        return Err(AppError::usage(format!(
            "input {:?} is not a directory",
            args.input
        )));
    }
    let input = std::fs::canonicalize(&args.input)
        .map_err(|e| AppError::usage(format!("cannot resolve input {:?}: {e}", args.input)))?;

    // Output: make absolute (and canonical as far as it exists).
    let out = absolutize(&args.out)?;
    if out.exists() && !out.is_dir() {
        return Err(AppError::usage(format!(
            "output {out:?} is not a directory"
        )));
    }

    // Overlap checks (both directions) — prevents reprocessing/output-in-input.
    if out == input || out.starts_with(&input) {
        return Err(AppError::usage(format!(
            "output {out:?} must not be inside input {input:?}"
        )));
    }
    if input.starts_with(&out) {
        return Err(AppError::usage(format!(
            "input {input:?} must not be inside output {out:?}"
        )));
    }

    // Template: unknown variables are a hard error before anything runs.
    let template = Template::parse(&args.template).map_err(AppError::Usage)?;

    let jobs = if args.jobs == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    } else {
        args.jobs.min(1024)
    };

    // Overrides are normalized up-front (they follow the usual rules).
    let mut overrides = HashMap::new();
    let mut add = |k: MetaKey, v: Option<String>| {
        if let Some(v) = v {
            let v = if k == MetaKey::Comment {
                v
            } else {
                normalize(&v)
            };
            if !v.trim().is_empty() {
                overrides.insert(k, v);
            }
        }
    };
    add(MetaKey::Band, args.band);
    add(MetaKey::Artist, args.artist);
    add(MetaKey::AlbumArtist, args.albumartist);
    add(MetaKey::Album, args.album);
    add(MetaKey::Year, args.year);
    add(MetaKey::Date, args.date);
    add(MetaKey::Title, args.title);
    add(MetaKey::Track, args.track);
    add(MetaKey::Disc, args.disc);
    add(MetaKey::Genre, args.genre);
    add(MetaKey::Artists, args.artists);
    add(MetaKey::Composer, args.composer);
    add(MetaKey::Comment, args.comment);

    Ok(Config {
        input,
        out,
        template,
        dry_run: args.dry_run,
        copy: args.copy,
        update_meta: args.update_meta,
        jobs,
        omit_success: args.omit_success,
        overrides,
    })
}

/// Make a path absolute, canonicalizing the deepest existing ancestor so
/// overlap checks are correct even for a not-yet-created output folder.
pub fn absolutize(p: &Path) -> Result<PathBuf, AppError> {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map_err(AppError::Io)?.join(p)
    };
    if let Ok(c) = abs.canonicalize() {
        return Ok(c);
    }
    // Walk up to the nearest existing ancestor.
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = abs.as_path();
    loop {
        if let Ok(meta) = std::fs::metadata(cur)
            && meta.is_dir()
        {
            let mut res = cur
                .canonicalize()
                .map_err(|e| AppError::usage(format!("cannot resolve {p:?}: {e}")))?;
            for c in suffix.iter().rev() {
                res.push(c);
            }
            return Ok(lexically_normalize(res));
        }
        match (cur.file_name(), cur.parent()) {
            (Some(name), Some(parent)) => {
                suffix.push(name.to_os_string());
                cur = parent;
            }
            _ => return Ok(lexically_normalize(abs)),
        }
    }
}

/// Remove `.` / resolve `..` without touching the filesystem.
fn lexically_normalize(p: PathBuf) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}
