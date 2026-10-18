use std::collections::HashMap;
use std::path::PathBuf;

/// Every metadata field the tool knows about. `Ext` exists only so templates
/// can reference `$ext`; it is never overridable and never tag-written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetaKey {
    Band,
    Artist,
    AlbumArtist,
    Album,
    Year,
    Date,
    Title,
    Track,
    Disc,
    Genre,
    Artists,
    Composer,
    Comment,
    Ext,
}

impl MetaKey {
    pub const FIELDS: [MetaKey; 13] = [
        MetaKey::Band,
        MetaKey::Artist,
        MetaKey::AlbumArtist,
        MetaKey::Album,
        MetaKey::Year,
        MetaKey::Date,
        MetaKey::Title,
        MetaKey::Track,
        MetaKey::Disc,
        MetaKey::Genre,
        MetaKey::Artists,
        MetaKey::Composer,
        MetaKey::Comment,
    ];

    pub fn name(self) -> &'static str {
        match self {
            MetaKey::Band => "band",
            MetaKey::Artist => "artist",
            MetaKey::AlbumArtist => "albumartist",
            MetaKey::Album => "album",
            MetaKey::Year => "year",
            MetaKey::Date => "date",
            MetaKey::Title => "title",
            MetaKey::Track => "track",
            MetaKey::Disc => "disc",
            MetaKey::Genre => "genre",
            MetaKey::Artists => "artists",
            MetaKey::Composer => "composer",
            MetaKey::Comment => "comment",
            MetaKey::Ext => "ext",
        }
    }

    pub fn from_name(name: &str) -> Option<MetaKey> {
        MetaKey::FIELDS
            .iter()
            .chain(std::iter::once(&MetaKey::Ext))
            .find(|k| k.name() == name)
            .copied()
    }

    /// Critical fields: when the template uses one and it cannot be resolved,
    /// the file is left unprocessed (SPEC "missing data resolution").
    pub fn is_critical(self) -> bool {
        matches!(self, MetaKey::Band | MetaKey::Album | MetaKey::Title)
    }
}

/// Which source supplied a field. Resolution precedence: Override > Tag > Path
/// is realized by applying the sources in reverse order (later wins).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Override,
    Tag,
    Path,
}

/// Final, normalized metadata for one file, with provenance per field
/// (provenance drives `--update-meta`).
#[derive(Debug, Clone, Default)]
pub struct ResolvedMeta {
    pub fields: HashMap<MetaKey, String>,
    pub origins: HashMap<MetaKey, Source>,
    /// Set when the tag layer failed to read the file (for better reasons).
    pub tag_error: Option<String>,
}

impl ResolvedMeta {
    /// Insert/overwrite with a non-empty value.
    pub fn put(&mut self, key: MetaKey, value: String, source: Source) {
        if value.trim().is_empty() || key == MetaKey::Ext {
            return;
        }
        self.fields.insert(key, value);
        self.origins.insert(key, source);
    }

    pub fn get(&self, key: MetaKey) -> Option<&str> {
        self.fields
            .get(&key)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }

    /// `$band`: an explicit `--band` override wins outright; otherwise the
    /// first non-empty of artist → albumartist → band (SPEC resolution order).
    pub fn band(&self) -> Option<&str> {
        if self.origins.get(&MetaKey::Band) == Some(&Source::Override)
            && let Some(v) = self.get(MetaKey::Band)
        {
            return Some(v);
        }
        self.get(MetaKey::Artist)
            .or_else(|| self.get(MetaKey::AlbumArtist))
            .or_else(|| self.get(MetaKey::Band))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileClass {
    Audio,
    Image,
    Junk,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ScannedFile {
    /// Absolute source path.
    pub src: PathBuf,
    /// Path relative to the input root (the deterministic sort key).
    pub rel: PathBuf,
    pub class: FileClass,
    /// Lower-cased extension without the dot; empty when absent.
    pub ext: String,
    pub size: u64,
    pub symlink: bool,
}

#[derive(Debug, Clone)]
pub enum PlannedOp {
    Move {
        dst: PathBuf,
    },
    Copy {
        dst: PathBuf,
    },
    /// Destination already exists with identical content (comparison happened
    /// at plan time). Move mode: source removed after re-verification.
    SkipIdentical {
        dst: PathBuf,
        md5: String,
    },
    /// Redundant input duplicate of a group leader destined for `dst`.
    /// Move mode only: source removed after destination verification.
    Dedup {
        dst: PathBuf,
        kept_md5: String,
    },
    Unprocessed {
        reason: String,
    },
    /// Non-audio noise: no destination, never moved, never affects the exit
    /// code; deleted during move-mode pruning when its folder is prunable.
    Junk,
}

#[derive(Debug, Clone)]
pub struct PlanEntry {
    pub file: ScannedFile,
    pub op: PlannedOp,
    /// md5 of the source when a conflict forced us to hash it.
    pub md5: Option<String>,
    pub meta: Option<ResolvedMeta>,
}

#[derive(Debug)]
pub struct Plan {
    pub entries: Vec<PlanEntry>,
    /// Every directory under the input root (candidates for pruning).
    pub dirs: Vec<PathBuf>,
    pub input_root: PathBuf,
    pub out: PathBuf,
    pub dry_run: bool,
    pub copy_mode: bool,
    pub update_meta: bool,
    pub jobs: usize,
    /// Files actually md5-hashed while planning (0 on a conflict-free run).
    pub hashes_computed: usize,
}

impl Plan {
    pub fn entry(&self, idx: usize) -> &PlanEntry {
        &self.entries[idx]
    }
}
