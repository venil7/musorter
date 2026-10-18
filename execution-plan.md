# musorter — Execution Plan

Technical build plan derived from `SPEC.md` (resolved). This is the document
the implementation is built from: crate choices, module layout, data model,
phase-by-phase algorithms, concurrency model, test strategy, milestones.

---

## 1. Goals / definition of done

- `musorter <INPUT> -o <OUT>` sorts an arbitrary music tree per SPEC, default
  or custom template, deterministic + idempotent.
- All behaviors in SPEC.md implemented: path-fallback metadata, normalization,
  critical/optional missing-data rules, md5 conflict/dedup logic, album art,
  junk handling, pruning bubble-up incl. root, `--dry-run` (zero writes),
  `--copy` (no pruning), `--update-meta` (processed files only), global
  overrides, report lines + summary, exit codes 0/1/2.
- `cargo test` green (unit + integration), `cargo clippy -- -D warnings`
  clean, builds for linux/macos/windows.

## 2. Toolchain & dependencies (verified against crates.io, Oct 2026)

| Crate | Version | Purpose |
|---|---|---|
| `clap` (derive) | 4.x | CLI parsing; explicit `Option<String>` field per override key (clap has no dynamic flags) |
| `tokio` (`rt-multi-thread`, `macros`) | 1.x | runtime; all blocking FS/tag/hash work goes through `spawn_blocking` behind a bounded semaphore |
| `lofty` | **0.25.4** | tag read+write: AAC, AIFF, APE, FLAC, MP3, MP4/M4A, MPC, Opus, Ogg Vorbis, Speex, WAV, WavPack (verified: actively maintained, edition 2024) |
| `walkdir` | 2.x | input scan (no symlink following) |
| `indicatif` | latest | the two stderr progress bars (map building + execution); hidden automatically when stderr is not a terminal |
| `md-5` | 0.10.x | md5 checksums (SPEC-mandated), `hex` 0.4 for formatting |
| `thiserror` | 2.x | typed errors; `anyhow` only at `main()` boundary |
| `tempfile` | dev | integration-test sandboxes |
| `assert_cmd` + `predicates` | dev | CLI integration tests |

**WMA/ASF**: lofty does *not* support ASF (confirmed via docs.rs + lofty ADR),
and there is **no maintained ASF crate** (`audiotags` unmaintained since 2024,
`unstable_asf` doesn't exist on crates.io). → implement `src/tags/asf.rs`,
a minimal in-house ASF reader/writer (see §8.2). This is the only significant
"write your own parser" item in the build.

No rayon: tokio's blocking pool with a `Semaphore(--jobs)` is sufficient and
keeps one concurrency story.

## 3. Project layout

```
Cargo.toml                # package name: musorter (bin name musorter), edition 2024
src/
  main.rs                 # tokio::main, arg parse, run(), exit-code mapping
  cli.rs                  # clap derive struct + override-key validation
  error.rs                # AppError (fatal, exit 2) vs FileError (fail-soft, exit 1)
  model.rs                # core types (§5)
  scan.rs                 # phase 1: walk + classify
  meta/
    mod.rs                # resolution orchestration (override > tag > path)
    tags.rs               # lofty adapter: read/write MetaBag
    asf.rs                # WMA/ASF read/write (in-house)
    infer.rs              # path/filename inference heuristics
    normalize.rs          # normalization pipeline (pure functions)
  template.rs             # template parse (→ AST) + render + critical check
  sanitize.rs             # path-segment sanitization
  hash.rs                 # chunked md5 (blocking-friendly)
  plan.rs                 # phase 2: build Plan (dedup + conflict allocation)
  exec.rs                 # phase 3: execute plan (move/copy/tag-write)
  prune.rs                # phase 4: junk + empty-folder bubble-up
  report.rs               # phase 5: line report, summary, exit code
tests/
  integration.rs          # end-to-end CLI tests (tempfile sandboxes)
  fixtures/               # tiny real audio files (generated once, committed)
```

Unit tests live in each module (`#[cfg(test)]`), normalization golden tests
in `meta/normalize.rs`, template tests in `template.rs`.

## 4. CLI

```rust
#[derive(Parser)]
#[command(name = "musorter", version, about)]
struct Args {
    /// Input folder
    input: PathBuf,
    /// Output folder (required)
    #[arg(short = 'o', long = "out")]
    out: PathBuf,
    #[arg(short = 't', long)]
    template: Option<String>,           // default: {$band}/{$year - $album}/{$track - $title}.{$ext}
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    copy: bool,
    #[arg(long)]
    update_meta: bool,
    #[arg(long, default_value_t = 0)]
    jobs: usize,                        // 0 = available_parallelism()
    #[arg(long)]
    omit_success: bool,
    // one explicit flag per overridable key (SPEC key list):
    #[arg(long)] band: Option<String>, artist: Option<String>,
    #[arg(long)] albumartist: Option<String>, album: Option<String>,
    #[arg(long)] year: Option<String>,  date: Option<String>,
    #[arg(long)] title: Option<String>, track: Option<String>,
    #[arg(long)] disc: Option<String>,  genre: Option<String>,
    #[arg(long)] artists: Option<String>, composer: Option<String>,
    #[arg(long)] comment: Option<String>,
}
```

Pre-validation (fatal, exit 2, before any I/O beyond arg parsing):

1. template parses; **unknown `$var` → exit 2**
2. `input` exists and is a directory
3. `out != input`, `out` not inside `input`, `input` not inside `out`
   (canonicalized where they exist; the not-yet-existing `out` is checked
   against canonicalized `input` prefix)
4. `jobs` sane (≤ 1024)

`--dry-run` additionally forces: no tag writes, no mkdir, no prune, no
deletes — enforced structurally (phases 3–4 are simply not invoked) and
guarded with a debug assertion in exec/prune entry points.

## 5. Data model

```rust
enum MetaKey { Band, Artist, AlbumArtist, Album, Year, Date, Title,
               Track, Disc, Genre, Artists, Composer, Comment, Ext }

/// Which source supplied a field (for --update-meta decisions + debugging).
enum Source { Override, Tag, Path }

struct MetaBag {
    fields: HashMap<MetaKey, String>,   // post-normalization values
    origins: HashMap<MetaKey, Source>,
    track_raw: Option<u32>,             // numeric forms kept for zero-padding
}

enum FileClass { Audio, Image, Junk, Unknown }

struct ScannedFile {
    src: PathBuf,                       // absolute
    rel: PathBuf,                       // relative to input root (sort key)
    class: FileClass,
    ext: String,                        // lower-cased, no dot
    dir_depth: u32,
}

enum PlannedOp {
    Move   { dst: PathBuf },
    Copy   { dst: PathBuf },
    SkipIdentical   { dst: PathBuf },   // dst holds same md5
    Dedup  { dst: PathBuf, keep: PathBuf }, // redundant input copy (move mode)
    Unprocessed { reason: String },     // incl. junk: "junk file"
}

struct PlanEntry { file: ScannedFile, op: PlannedOp, md5: Option<String> }

struct Plan {
    entries: Vec<PlanEntry>,            // sorted by file.rel (byte order)
    dirs: Vec<PathBuf>,                 // all input dirs (for prune), absolute
    mode_write: bool,                   // false ⇐ dry-run
    copy_mode: bool,
    stats_pre: …,                       // counters filled during execution
}
```

Only audio/image entries can carry a destination; `Unknown` class always
becomes `Unprocessed { "unsupported file type" }`.

## 6. Pipeline

```
main → parse+validate → P1 scan → P2 meta resolve → P3 normalize+overrides
     → P4 render template → P5 conflict detection + lazy md5 + allocation → PLAN
     → dry-run? ──yes──> report, exit
     → execute (§7) → prune (§9) → report (§10) → exit code
```

### P1 — scan

- `WalkDir::new(input).follow_links(false).sort_by_file_name()` — results are
  re-sorted globally afterwards anyway; symlinked dirs are not descended and
  symlinked files are reported unprocessed (`symlink`) — safety over
  completeness.
- Classify by extension: audio set (§8), image set, junk set (§SPEC),
  else `Unknown`. Record every directory for the prune phase.
- Hidden/system files: classified by name/extension like everything else
  (`.DS_Store`, `Thumbs.db`, `desktop.ini` → Junk).

### P2 — metadata resolution (per file, parallel)

Order per field: **Override → Tag → Path inference** (SPEC precedence).

1. `overrides`: pre-built `HashMap<MetaKey, String>` from CLI (normalized in P3
   like any value, `Source::Override`).
2. Tags: `meta/tags.rs` via lofty
   `lofty::config::ParseOptions { .. tolerant: true }` —
   `read_from_path` → primary tag → map frames to `MetaKey`:
   - `Title→Title`, `TrackArtist→Artist`, `Album→Album`,
     `AlbumArtist→AlbumArtist`, `TrackNumber→Track`, `DiscNumber→Disc`,
     `Genre→Genre`, `Composer→Composer`, `EnclosedLyrics/…` no,
     `Comment→Comment`, `RecordingDate→Year` (leading 4 digits) / `Date`,
     freeform `BAND→Band`, `ARTISTS→Artists`.
   - tag read error → treat tags as absent (`tolerant`), fall through to path
     inference; remember error to improve the unprocessed reason if the file
     ends up unprocessable.
3. Path inference (`meta/infer.rs`) fills only missing fields — rules exactly
   as SPEC (filename `NN[. _-]Title` → track/title; `^\d{4}` ancestor → year;
   next non-year ancestor → album; next → artist).
   Working set: ancestors of `rel` up to input root, nearest first.
4. `$band` derived at render time: first non-empty of
   `band ← artist, albumartist, band` (respecting each field's own override).

### P3 — normalization (pure, applied to every value regardless of source)

`normalize.rs::normalize(&str) -> String`, steps per SPEC:

1. `unicode-normalize` NFC (via `lofty`'s re-export or direct dep on
   `unicode-normalization`); drop `char::is_control()`.
2. `_` → space; whitespace runs → single space.
3. Word splitting into segments (split *points*, characters preserved):
   whitespace, `-`, lower→Upper (`char::is_lowercase`→`is_uppercase`),
   letter↔digit. Implementation: iterate chars with previous-char state,
   emit boundary before `c` when transitions apply.
4. Title-case per segment: skip non-alphanumerics (apostrophes etc.) when
   finding first alpha, uppercase it, lowercase all remaining *cased* chars
   of the segment.
5. Trim + collapse (idempotent: `normalize(normalize(x)) == normalize(x)` —
   asserted by a property test).

`track` additionally normalized numerically: parse leading digits → `u32`,
render zero-padded `{:02}` (widens naturally past 99).

### P4 — template (`template.rs`)

Parse once at startup:

```rust
enum Part { Lit(String), Var(MetaKey) }
struct Section(Vec<Part>);           // '/'-separated; last = file name
struct Template { sections: Vec<Section> }
```

- `$name` scan against the known-key table; **unknown → AppError(exit 2)**.
- Last section must yield an extension: either literal `.ext` or `Var(Ext)`.
  Validation error → exit 2.

Render for a file:

1. **Critical check first**: for each `Var(k)` present in the template where
   `k ∈ {Band, Album, Title}` and resolved value is empty →
   `Unprocessed { format!("missing {}", key_name(k)) }`.
2. Render each section: concatenate Lit+values; a missing *optional* Var is
   removed together with adjacent glue — after substitution, collapse any
   occurrence of whitespace-`-`-whitespace that ends up adjacent to a removed
   value, then trim. Equivalent formulation used in code: render with an
   `EMARK` sentinel around omitted vars, remove `sentinel + surrounding glue`
   matching `^\s*-\s*$` patterns, then strip sentinels.
   Examples: `{Year - Album}` with no year → `Album`;
   `{Track - Title}` with no track → `Title.ext`.
3. Empty section after omission → drop the section (no empty dir).
4. Sanitize each resulting segment (`sanitize.rs`): replace
   `/ \ : * ? " < > |` + controls with space → collapse → trim space/dots →
   150-char UTF-8-safe truncation (then trim dots again) → reserved device
   names `_`-prefixed → empty ⇒ drop (file-name section ⇒ `_`).
   Extension: lower-cased, appended verbatim.
5. Result: `dst = out.join(rel_path)`.

**Album art rendering**: run only the *directory* sections (all but last)
through the template using path-inferred meta, then append the original
(sanitized) image file name — `folder.jpg` stays `folder.jpg`. Critical check
applies to `Band`/`Album` as usual (`Title` isn't used for images);
if band/album can't be inferred → `Unprocessed { "cannot determine album" }`.

### P5 — consolidation, rendering, conflict detection, lazy hashing, allocation

**md5 is computed only for files involved in a destination conflict** (SPEC
clarification): a file whose rendered destination is free (and shares its
destination key with no other input) is moved/copied **without ever being
hashed**. The phase runs in deterministic stages over entries sorted by `rel`
(byte order of components). The whole phase is covered by progress-bar
phase 1/2 (see §11a).

**Stage A0 — band/album consolidation (SPEC "album & band consolidation"):**
after metadata resolution, cluster the resolved values:

- `cluster(values)`: unique values sorted by (byte-length, bytes); each value
  is claimed by the first (⇒ shortest) established representative `r` with
  `v.starts_with(r) && (v.len() == r.len() || v[r.len()] == ' ')`; values
  with no match become representatives themselves. O(n·m) over small unique
  sets, fully deterministic, order-independent by construction.
- bands cluster globally; albums cluster **per consolidated band** (bucket
  key = the file's post-consolidation `$band`, `""` when absent).
- rendering uses a **path-only clone** of the metadata with the band-chain
  fields / `Album` replaced by their representatives; `PlanEntry.meta`
  keeps the original values so `--update-meta` never erases editions.

**Stage A1 — render destinations (two passes):**
1. **Audio** entries render the full template → candidate `dst`.
2. **Album art** (`FileClass::Image`): if its *source directory* contains
   audio entries that rendered successfully, the image goes to the **most
   common audio destination directory** in that folder (ties broken
   lexicographically) + its sanitized original name — art always lands
   beside its sibling songs even when those were placed from tags (bug fix:
   path-inferred art used to drift into a folder mirroring the input
   structure, e.g. `out/Music/Band/…`). Otherwise fall back to rendering the
   template's directory sections from the image's own path-inferred
   metadata (`cannot determine album` when that fails).
3. `Unknown` → `unsupported file type` / `symlink`; `Junk` → no destination.

**Stage A2 — disk occupancy (serial, cheap):** for every unique rendered
parent directory, `read_dir` once → map of `lowercased(full path) → (path,
size)`. This makes occupancy lookups case-insensitive-aware on every
platform (`Cover.jpg`/`cover.jpg` collide on Linux too) without walking the
whole output tree.

**Stage A3 — conflict detection (serial, stats only, no file reads):**
1. Group rendered entries by destination key = `lowercased(dst)`. Groups
   with **>1 member** are an *input conflict* — all members will need md5.
2. For each group, probe the suffix chain `base`, `base-1`, `base-2`, …
   **stat only** until the first free slot (stage-A2 records for anything in
   an occupied parent). A candidate whose **size differs** from a source is
   provably a different file — **no md5 needed**; a candidate with size
   equal to a source marks both (source + destination file) for hashing.
3. Output: the set of paths to hash (conflict participants only).

**Stage B — hashing (parallel):** chunked md5 (64 KiB) for exactly the stage-A
set, inputs and existing destination files alike, via `spawn_blocking` under
the `--jobs` semaphore, results merged into a `HashMap<PathBuf, md5>`. No
ordering dependency — content-addressed.

**Stage C — allocation (serial, pure logic + rare on-demand stats/hashes):**
iterate entries in sorted-`rel` order; for each, probe the suffix chain
consulting, in order:
- **in-run occupancy map** (`BTreeMap<lowercased dst, entry>` — includes
  candidates allocated earlier in this run);
- **on-disk state** (stage-A records first, live `stat` beyond them).

Probe outcomes:
- slot free (neither in-run nor on disk) → allocate: `Move/Copy { dst }`,
  register in the occupancy map;
- occupied with **equal md5** (hashes from stage B, or computed on demand for
  exotic suffix-chain collisions — memoized) →
  - input duplicate (in-run occupant): move mode → `Dedup { dst, keep }`,
    copy mode → `SkipIdentical { dst }`
  - existing destination file: `SkipIdentical { dst }`
- occupied with **different content** (proven by differing md5, or by
  differing size without hashing) → try `stem-1.ext`, `stem-2.ext`, …
  (SPEC suffix rule) until a free or identical slot is found.

Group leader = first member allocated to a given destination in sorted order;
all later same-md5 members become `Dedup`/`SkipIdentical` as above.
Plan is now complete. Nothing on disk has been modified, and in the
common case (no conflicts) **zero bytes were hashed**.

Idempotency follows: same inputs + same existing-output state ⇒ same plan;
second run after a successful run has an empty input ⇒ empty plan.

## 7. Execution (`exec.rs`)

- `Semaphore::new(jobs)` + `JoinSet`; each task handles one entry via
  `spawn_blocking` (std fs, never async fs — batched blocking is the tokio
  guidance). Results collected as `Vec<(idx, Result<ActionLine, FileError>)>`,
  then sorted by `idx` ⇒ report order is deterministic regardless of
  scheduling.
- **Progress bar 2** covers the whole execute() call: length = wave-1
  runnable entries + wave-2 dedups, incremented as results come in,
  `finish_and_clear()` before the report is written (see §11a).
- **`update-meta` step (audio only, processed files only)**:
  - **move mode**: write tags on the *source* path immediately before the
    move; a write failure ⇒ `FileError`, file left byte-identical as-is,
    reported, exit 1 (run stays retryable);
  - **copy mode**: copy first, then write tags on the *destination* copy —
    the source is never modified; a write failure ⇒ the freshly written
    destination copy is removed, source untouched, reported, exit 1;
  - write only resolved non-empty values that differ from stored tags;
    never delete unknown frames (lofty `save_to_path` preserves untouched
    items; ASF module preserves unknown descriptors);
  - if the entry carries a planned md5 (conflict case), re-hash after a
    successful write and update the plan entry before any dedup/deletion
    decisions depend on it;
  - never runs in dry-run; images/junk never tag-written.
- **Move**: `fs::rename(src, dst)`; on `EXDEV` (cross-device) fallback:
  copy → verify (size always; md5 when the plan has one) → `remove_file(src)`.
  Source is **only** deleted after verification. Create parent dirs
  (`create_dir_all`) immediately before the operation — never in dry-run.
- **Copy**: copy → verify (size always; md5 when planned) → done; source
  untouched.
- **SkipIdentical (dest already has content)**: move mode → verify dest md5
  then `remove_file(src)` (reported `deduplicated: dst, source: src`);
  copy mode → no filesystem action (reported `skipped (identical): …`).
- **Dedup**: only delete a redundant source **after** the group leader's
  move/copy has succeeded (verified). Leader failed ⇒ duplicates stay and are
  reported unprocessed (`leader failed`).
- **Unprocessed**: no filesystem action, reason recorded.
- Per-file failures (permissions, I/O) are fail-soft: recorded, run continues.

## 8. Tag layer

### 8.1 lofty adapter (`meta/tags.rs`)

- Read: `lofty::read_from_path` with tolerant parse options; map primary tag
  frames → `MetaBag` (table in P2). ID3v1-only files work (lofty handles);
  `RecordingDate` sliced to leading 4 digits for `Year`.
- Write: set only the keys we resolved to a non-empty value *and* that differ
  from stored; `tag.save_to_path(path, WriteOptions::default())`.
- Formats where lofty marks a *secondary* tag read-only (FLAC+ID3v2, MPC+ID3):
  always write the format's primary writable tag type.
- `AAC (ADTS)`, `MP4`, `Ogg`, `Opus`, `WAV`, `AIFF`, `APE`, `WavPack` — all
  covered by lofty with write support for their primary tag.

### 8.2 ASF/WMA module (`meta/asf.rs`) — in-house

ASF layout: `Header Object` (GUID + u64 size) containing ordered sub-objects;
the two we need are `Content Description Object` (title/author/copyright/
description/rating — UTF-16LE strings) and `Extended Content Description
Object` (key/value descriptor list — where WMA-specific keys live:
`WM/AlbumTitle`, `WM/Artist`, `WM/TrackNumber`, `WM/Genre`, `WM/Year`,
`WM/Composer`, `WM/AlbumArtist`…).

- **Read** (~250 LOC): validate header GUID, walk sub-objects by their
  declared sizes, parse the two descriptor objects, map into `MetaBag`
  (`Author→Artist`, etc.). Bounded: reject sizes past EOF, cap string lengths.
- **Write** (~300 LOC): parse → rebuild the two descriptor objects with
  updated values (UTF-16LE, descriptor-count and size fields recomputed) →
  cascade size updates: sub-object sizes → `Header Object` size →
  `TotalDataSize` in File Properties → splice: write the new header into a
  temp file, stream-copy the remaining payload, atomically replace.
- **Fallback policy** (risk containment): if write hits an unhandled ASF
  variant → return `FileError` ⇒ file left alone + reported (exit 1), never a
  crash and never a truncated file (all writes are temp-file + rename).
- Tests: fixture `.wma` (generated once with ffmpeg, committed) round-trip
  read + write + re-read.

## 9. Pruning (`prune.rs`) — move mode only, after execution

Input: `plan.dirs` (every directory found in P1) + set of files that remained
(unprocessed/failed sources).

1. Sort dirs by depth **descending** (innermost first), tie-break lexicographic.
2. For each dir still existing:
   - read it; if it contains entries only of type *lingering source files*
     (fail-soft leftovers) → keep.
   - if empty → `remove_dir` → report line for the folder? No — folders are
     not reported (report covers files); count silently.
   - if **all remaining files are Junk** (SPEC junk list) → delete those junk
     files (report each as `removed (junk): path`) → `remove_dir`.
   - otherwise → keep (it has real leftovers).
3. Bubble-up is automatic: a parent becomes empty once children are removed
   and its own files were processed. The **input root follows the same rule**
   and may be deleted; if leftovers exist anywhere, that path and every
   ancestor directory along the way to the root remain.
4. Never runs for `--dry-run` or `--copy`.

## 10. Reporting (`report.rs`)

Line format per SPEC: `{what happened}: {path}` (+ `, source: {old}` for
transformations).

| Verb | Emitted when |
|---|---|
| `moved: {dst}, source: {src}` | move succeeded |
| `copied: {dst}, source: {src}` | copy succeeded |
| `deduplicated: {dst}, source: {src}` | move mode: content identical to `{dst}` already exists there (dest hit *or* input duplicate) ⇒ source removed |
| `skipped (identical): {dst}, source: {src}` | copy mode: identical content already at/going to `{dst}` ⇒ source untouched |
| `removed (junk): {src}` | junk deleted during prune (move mode) |
| `{reason}: {src}` | unprocessed — reasons: `missing band|album|title`, `cannot determine album`, `unsupported file type`, `symlink`, `unreadable tags`, `tag write failed: {e}`, `io error: {e}`, `leader failed` |

- Order: all lines sorted by source path (byte order) → deterministic.
- `--omit-success`: print only `{reason}: …` lines (and errors).
- **Junk never affects the exit code**: in move mode deleted junk prints
  `removed (junk): {src}`; junk that survives (copy mode, dry-run, or a
  folder kept alive by leftovers) prints `ignored (junk): {src}`.
- Dry run prints the identical line stream it *would* produce; verbs match
  the selected mode (`moved:` default, `copied:` with `--copy`).
- Summary block: processed count (number only), copied/moved, skipped
  identical, deduplicated, junk removed, **full unprocessed list** (already
  printed as lines; summary gives counts + totals).

Exit code: any unprocessed/error ⇒ 1; fatal validation/IO-of-plan ⇒ 2;
else 0.

## 11. Concurrency & determinism

### 11a. Progress indication (SPEC "progress indication")

- `indicatif` bars drawn on **stderr** only when stderr is a terminal
  (`std::io::IsTerminal`); otherwise `ProgressBar::hidden()` — tests, pipes
  and redirected logs never see progress bytes.
- **Bar 1 (map building)** lives in `plan::build()`:
  phase 1 `loading metadata` (length = number of resolvable files,
  incremented by `resolve_all` tasks), then phase 2 `planning destinations`
  (position reset, length = all files, incremented in the allocation loop),
  `finish_and_clear()` when the plan is ready.
- **Bar 2 (execution)** lives in `exec::execute()` as described in §7.
- Shared helper `ui::progress(len, msg)` centralizes the TTY check + style.

- Single tokio multi-thread runtime; `--jobs` semaphore bounds *all* blocking
  work (tag read, hashing, copy/move). Default = `available_parallelism()`.
- Plan-building phases are parallel *per file* but produce indexed results;
  the plan vector is assembled/sorted by `rel` before any decision that
  depends on order (dedup leadership, conflict allocation) — scheduling never
  affects outcomes.
- Execution results sorted by index before reporting.
- Shared state during allocation is a single-threaded pass (no locks);
  execution phase uses no shared mutable state (each entry owns its paths;
  the dest map is frozen at plan time).

## 12. Safety invariants (encoded as code + tests)

1. Dry-run ⇒ zero disk writes (test: full tree snapshot incl. mtimes
   unchanged).
2. Source deleted only after destination verified (size; md5 when the plan
   hashed the file — always the case for dedup/identical decisions).
3. `--copy` ⇒ no deletions, no pruning, sources byte-identical.
4. Unprocessed files byte-identical even with `--update-meta` (hash+mtime
   assertion).
5. Tag writes never on images/junk; never in dry-run.
6. Determinism: `plan(rel)` is a pure function of (input tree, output tree,
   args) — asserted by running the planner twice with shuffled walk order.
7. Idempotency: run → run again ⇒ second run reports 0 moves, exit 0.

## 13. Testing strategy

**Unit** (in-module):
- `normalize`: golden table incl. every SPEC example
  (`Master OF PUPPETS`, `BlackAlbum`, `Kill-'Em-All`, `RideTheLighting`,
  `METALLICA`, underscores, `'em`, unicode) + idempotency property
  (`n(n(x)) == n(x)`).
- `template`: default render matrix over missing-optional combos (year, track
  combos), critical-missing ⇒ unprocessed, unknown var ⇒ exit-2 error,
  empty-section dropping, glue collapse (`" - "` never orphaned).
- `sanitize`: reserved names, 150-char UTF-8 truncation, `<>:"/\|?*`,
  trailing dots, non-ASCII.
- `infer`: the SPEC folder examples, `01 - Title`, `1985 - Live` year dirs,
  shallow trees (no album/artist ancestor), and **trailing-paren preservation**
  (`(1985) Great Hits (Deluxe)` ⇒ album keeps `(Deluxe)` — `strip_sep` bug
  fix: only `-`/`_`/space are separator noise, brackets are content).
- `consolidate`: the SPEC grouping examples (`album`/`album (Japanese
  edition)`/`album extra` ⇒ `album`; `abc`/`abc whatever` ⇒ `abc`;
  `abc` vs `abcde` stay distinct), chain/order independence.
- `plan`: dedup leadership, suffix allocation incl. case-insensitive keys,
  existing-dest same/different md5.
- `asf`: byte-level fixtures for both descriptor objects.

**Integration** (`tests/integration.rs`, binary invoked via `assert_cmd`,
sandbox via `tempfile`):
- fixtures: tiny real files for mp3 / flac / m4a / ogg / opus / wma / aac,
  generated once with ffmpeg (`tests/gen_fixtures.sh`, committed, <100 KB
  total) and tagged in-test via lofty/asf writer.
- e2e move run: assert exact output tree + report lines + exit 0 + input gone.
- **consolidation e2e**: `Band/Album/…` + `Band/Album (Japanese edition)/…`
  both render into `out/Band/Album/`.
- **album-art alignment e2e**: tagged song + `cover.jpg` side by side ⇒ both
  land in the tag-derived album directory (art no longer re-derives its own
  path); art-only folders still fall back to path inference.
- dry-run: snapshot equality (names, sizes, mtimes, hashes).
- copy mode: source tree intact, no dirs pruned.
- conflicts: pre-seeded dest with different bytes ⇒ `-1` suffix; identical
  dest ⇒ skipped; three-way input duplicates ⇒ one output.
- missing critical (no album, no override) ⇒ untouched; `--album X` ⇒
  processed into `X`.
- overrides: `--year 1990` wins over tag + path; `--update-meta` persists it.
- `--update-meta`: written values readable back; unprocessed file untouched.
- prune: junk-only folder removed in move mode, kept in copy mode; leftover
  unsupported file keeps its ancestor chain alive.
- idempotency: second run ⇒ exit 0, empty report of moves.
- CLI: unknown template var / out-inside-in / missing `-o` ⇒ exit 2.

**CI**: GitHub Actions matrix (ubuntu/macos/windows) running
`cargo fmt --check`, `clippy -D warnings`, `cargo test` — Windows job matters
for reserved-name sanitization + case-insensitive allocation.

## 14. Milestones

> **Status (Oct 2026): M0–M9 all completed.** `cargo test` → 47 unit + 23
> integration tests green; `cargo clippy --all-targets -- -D warnings` and
> `cargo fmt --check` clean; CI workflow (`.github/workflows/ci.yml`) runs
> fmt/clippy on Linux + tests on Linux/macOS/Windows. Delivered deviations
> (all recorded here and in SPEC.md):
> - crate split into `lib.rs` + thin `main.rs` so integration tests exercise
>   the real implementation (needed for WMA round-trip verification);
> - `MUSORTER_DEBUG_STATS=1` prints `hashed=N` on stderr — the observable
>   proof of the "md5 only on destination conflicts" requirement (asserted
>   by `clean_run_never_hashes_and_conflict_run_does`);
> - ASF writer also understands the **Metadata Library Object**
>   (`D2D0A440-…`, WORD data lengths, null-terminated names) that ffmpeg
>   writes — verified byte-level — updating every descriptor container
>   present instead of shadowing it with a second one;
> - formats without a dedicated year frame (ID3v2 `TDRC` only) get the year
>   folded into the date field on write.

| # | Deliverable | Acceptance |
|---|---|---|
| M0 | Skeleton: Cargo (name `musorter`), cli.rs, error.rs, empty pipeline wiring, CI | `musorter --help` works; clippy clean |
| M1 | scan + classify + report of raw inventory | reports audio/image/junk/unknown correctly |
| M2 | normalize + sanitize + template render (pure core) | all unit golden tests pass |
| M3 | tag reading via lofty + path inference + overrides precedence | SPEC examples resolve from both tags and paths |
| M4 | plan: conflict detection, **lazy md5 (conflicts only)**, dedup, suffix allocation, case-insensitive map | planner unit tests + determinism test + no-hash-on-clean-run assertion |
| M5 | execute: move/copy/verify/EXDEV fallback + dry-run gate | e2e move/copy/dry-run integration tests |
| M6 | prune bubble-up + junk removal | prune tests (move vs copy vs leftovers) |
| M7 | `--update-meta` (lofty writes + overrides persistence) | round-trip tests; unprocessed untouched |
| M8 | ASF/WMA module read + write | wma fixture round-trip + e2e wma sort |
| M9 | report polish (`--omit-success`, summary), exit codes, docs (README) | full suite green on 3 OSes |

Ordering rationale: M2/M4 are pure logic (fast tests, no I/O risk) come
before any filesystem mutation; the riskiest item (ASF) is isolated in M8 and
cannot block the mainline (read-only fallback already shipped in M3/M7 —
`asf.rs` read half lands with M3, write half with M8).

## 15. Risks & mitigations

| Risk | Mitigation |
|---|---|
| ASF write complexity (cascading size fields) | temp-file + atomic rename; failure ⇒ fail-soft, file untouched, exit 1; thorough byte-level tests; read support lands earlier |
| lofty tag-coverage quirks (ID3v1-only, latin1, corrupt frames) | tolerant parse options; path inference as fallback; per-file fail-soft |
| Case-insensitive filesystem collisions (macOS/Windows) | lowercased keys in destination allocation map; CI on all 3 OSes |
| Cross-device moves | rename → EXDEV → copy+verify+delete-source fallback |
| Huge collections (100k files) | chunked md5, bounded semaphore, plan is O(n) memory (paths + 32-byte hashes — fine) |
| Nondeterminism from threads | indexed results + sort-by-`rel` before every order-dependent decision; determinism test |
| Data loss | invariants §12 encoded as tests; source deletion only post-verification; dry-run structurally unable to write |