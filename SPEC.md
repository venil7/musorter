# musorter — Specification (resolved)

```yaml
name: musorter
technology: Rust
type: CLI app, compiles against any target platform that Rust supports
```

## description

A music collection sorter. It takes an input folder containing an arbitrary
folder structure of music files, reads meta information (band name, album name,
song title, year, etc.) for each file, then **moves** files according to that
meta information into a folder structure and file naming scheme that is
unambiguously defined — calculated such that the same meta information always
produces the same output path (stable and idempotent for as long as the meta
information of the file has not changed).

Default output path layout:

```
{out-folder}/{band-name}/{year - album-name}/{track - title}.{ext}
```

The user may specify their own output path layout as a template string using
any meta data field: `$band`, `$album`, `$year`, `$track`, `$title`, ... etc.

Examples (input may be structured folders *and/or* carry tags — see
"meta data sources"): after normalization,

```
in:  Metallica/Master OF PUPPETS/1985/…  ->  Metallica/1985 - Master Of Puppets/…
in:  METALLICA/RideTheLighting/1987/…    ->  Metallica/1987 - Ride The Lighting/…
in:  metallica/Kill-'Em-All/1982/…       ->  Metallica/1982 - Kill-'Em-All/…
in:  metallica/BlackAlbum/…              ->  Metallica/Black Album/…
```

(the file-name part is `{track - title}.{ext}`, e.g. `03 - Master of Puppets.mp3`).

## command line interface

```
musorter <INPUT> -o <OUTPUT> [OPTIONS]
```

| Flag | Meaning |
|---|---|
| `<INPUT>` | positional: input folder (arbitrary structure) |
| `-o`, `--out <OUTPUT>` | output folder (**required**) |
| `-t`, `--template <TEMPLATE>` | custom output path template (default: `{$band}/{$year - $album}/{$track - $title}.{$ext}`) |
| `--dry-run` | render and print what would happen; **writes nothing to disk, ever** (no moves, no copies, no tag writes, no folder creation, no pruning) |
| `--copy` | copy instead of move (default is move) |
| `--update-meta` | write normalized meta data back into processed files (see below) |
| `--<key> <value>` | override any meta data field, globally for the entire run (e.g. `--year 1990`, `--album "Load"`, `--genre Metal`) |
| `--jobs <N>` | max concurrent workers (default: CPU count) |
| `--omit-success` | suppress success lines in the report; print only failures/unprocessed |

Overridable keys (= all known meta data fields):
`band, artist, albumartist, album, year, date, title, track, disc, genre,
artists, composer, comment`.

`$ext` is derived from the file itself and is not overridable.

## meta data sources and precedence

Meta data for a file is resolved in this order (first source that has a value
wins, **per field**):

1. **CLI overrides** — `--year 1990` etc. **always take precedence** over
   everything, for every file in the run. Override values are themselves run
   through the usual normalization rules before use.
2. **Tags** (preferred source) — read from the file's own meta tags
   (ID3v1/v2, Vorbis Comments, MP4 atoms, ASF, ...).
3. **Path/filename fallback** — if (and only if) a field is still missing, it
   is inferred from the input file name and its folder structure (best effort,
   see "path inference"). This is how untagged files and the folder examples
   above are processed.

Path inference heuristics (only fill *missing* fields, never override tags):

- filename without extension: `NN - Title` / `NN. Title` / `NN_Title` →
  `track` + `title`; otherwise the whole stem → `title`
- ancestor folder that looks like a year (`1985`, `1985 - Live`, `(1985)`) →
  `year` (plus album candidate from the remainder when present)
- first remaining ancestor folder above the file → `album`
- next ancestor folder → `artist` (feeds `$band` via the resolution order below)

`$band` resolves as first non-empty of: **`artist` → `albumartist` → `band`**
(the `--band` / `--artist` / `--albumartist` overrides apply before this).

Album art files have no tags — their meta is resolved purely from path
inference (plus overrides).

## meta data normalization

Before meta data is converted into a path, it is normalized **in memory**
(files themselves are not modified unless `--update-meta` is given). Rules:

1. Unicode NFC-normalize; remove control/non-printable characters.
2. Underscores → space. (Hyphens are **kept** — they are often part of a name
   or title — but they still act as word boundaries for casing.)
3. Split words at: whitespace, hyphens, lower→upper case transitions
   (`BlackAlbum` → `Black Album`), and letter↔digit transitions.
4. Collapse repeated whitespace, trim both sides.
5. **Title case** every word: first alphabetic character uppercased, the rest
   lowercased; an apostrophe is skipped when finding the first character
   (`'em` → `'Em`, `don't` → `Don't`). Small words are not special-cased:
   `OF` → `Of`. Non-cased scripts are left as-is.

Applied equally to tags, path-inferred values and CLI overrides, so that two
albums of the same band spelled differently end up in the same folder:

```
Master OF PUPPETS        -> Master Of Puppets
METALLICA                -> Metallica
BlackAlbum               -> Black Album
RideTheLighting          -> Ride The Lighting
Kill-'Em-All             -> Kill-'Em-All
  metallica/_the_black_  -> Metallica The Black
```

## output path template

### default template

```
{$band}/{$year - $album}/{$track - $title}.{$ext}
```

- `$track` renders zero-padded to at least 2 digits (`3` → `03`, `123` → `123`).
- No `" - "` artifacts are ever left behind for missing optional parts:
  year missing → section renders as `{album}`; track missing → file name
  renders as `{title}.{ext}`.

### variables

`$band $artist $albumartist $album $year $date $title $track $disc $genre
$artists $composer $comment $ext`

- Template syntax: `$name` placeholders, literal text otherwise, `/` separates
  path sections (the final section is the file name and must end with
  `.$ext` or carry a literal extension).
- **Unknown variable → hard error (exit 2) before any file is touched.**

### missing data resolution

- **Critical fields**: `band`, `album`, `title` — when the template *uses* such
  a field and it is missing after applying all resolution sources, the file is
  **left alone in its source folder** and listed as unprocessed
  (e.g. default template, `/{album}/` section, album missing → untouched).
  Providing the field via CLI parameter (`--album ...`) takes precedence and
  unblocks the file.
  If a critical field is **not referenced** by the chosen template, its absence
  does not prevent processing.
- **Optional fields** (`year`, `track`, `disc`, `genre`, ...): if missing, only
  that placeholder (with its adjacent `" - "` glue) is omitted — the file is
  still processed. Example: `{year is missing, album present}` →
  `{album}/…` — processed.
- If a whole path section becomes empty after omissions, the section is
  dropped entirely (no empty folders).

### path sanitization

After normalization, each path segment is made safe for all platforms:

- replace `/ \ : * ? " < > |` and control characters with a space, collapse
  whitespace, trim whitespace and dots from both ends
- cap each segment at 150 characters (UTF-8 safe, then re-trim trailing dots)
- Windows reserved device names (`CON PRN AUX NUL COM1..9 LPT1..9`, also with
  an extension) get a `_` prefix
- an empty result → segment is dropped (or `_` for the file-name section)
- the extension itself is preserved verbatim (lower-cased)

## idempotency

- Path construction is a pure function of (template, resolved+normalized meta,
  extension): the same input always produces the same output path.
- Running the tool twice over unchanged input produces the same plan; a second
  run over an already-sorted collection is a no-op (input empty / everything
  reported as already in place).
- All decisions are computed up-front over a deterministically ordered
  (source-path sorted) file list, so results and reports do not depend on
  thread scheduling.

## conflict resolution strategy

- If the destination file already exists, compare md5 checksums.
- **Same checksum** → it is the same file → the operation is skipped; in move
  mode the redundant source is removed after verification (reported), in copy
  mode the source is left in place.
- **Different checksum** → different files; the conflicting destination is
  suffixed with `-1`, `-2`, `-3`, … trying candidates until a free slot (or a
  slot holding identical content) is found:
  `Song.flac` → `Song-1.flac` → `Song-2.flac` → …
- **Duplicates within the input** (several sources with the same checksum
  mapping to the same destination): only the first (in sorted order) is moved;
  the redundant copies are removed in move mode (reported), left in place in
  copy mode. This is the *only* scenario in which input files disappear without
  a unique output counterpart — output still holds exactly one copy.
- Allocation is case-insensitive-aware (`Cover.jpg` vs `cover.jpg` collide on
  macOS/Windows) and considers both existing destination files and
  already-allocated in-run destinations.

## album art

- Separate image files (`jpg jpeg png gif webp bmp`) carry no tags; they are
  moved/copied into the album directory resolved from their path (artist/album
  inferred from their location, plus overrides), keeping their original file
  name (only sanitized — no title-casing of e.g. `folder.jpg`).
- If the album cannot be determined → left alone, reported.
- Subject to the same conflict rule (e.g. two different `cover.jpg` in one
  album → `cover-1.jpg`).

## other files

- **Junk files** (`.log .cue .m3u .m3u8 .pls .sfv .nfo .diz .txt .url` and
  system files `.DS_Store Thumbs.db desktop.ini`) are never moved to the
  output: they get no destination, are reported, and — in **move mode only** —
  are removed during the empty-folder cleanup (a folder containing nothing but
  junk counts as prunable, so cleanup can bubble up). In `--copy` mode and
  `--dry-run` nothing is pruned and junk stays untouched.
- **Unknown/unsupported extensions** are left alone + reported
  (`unsupported file type`) and block pruning of their folder.

## additional options

- `--dry-run` — renders the exact paths of how the move/copy would have
  happened. **Guaranteed zero writes to disk** (no moves, copies, tag writes,
  directory creation or pruning).
- Default behavior is **move**; `--copy` copies everything that would
  otherwise be moved and keeps whatever is left alone exactly as-is
  (**no pruning in copy mode**).
- `--update-meta` — persists normalized meta data **for processed files only**:
  - writes back all final field values, including bits inferred from the path
    and including CLI overrides (overrides always take precedence);
  - **unprocessed files are NEVER touched** — they stay byte-identical;
  - never runs under `--dry-run`;
  - tag writing happens **before** the move/copy of that file; if the write
    fails (e.g. read-only tag format), the file is left alone and reported as
    an error (exit 1) — no half-updated state, the run stays retryable.
- `--<key> <value>` overrides apply to the **entire input** of the run, are
  normalized as per the usual rules, take precedence over tags and path
  inference, and (with `--update-meta`) are persisted.

## files and tags supported

Supported containers (anything the tag layer can read; extensible):

- **mp3** (ID3v1, ID3v2.2/2.3/2.4, APE)
- **aac** (ADTS, ID3) and **m4a / mp4** (MP4 atoms)
- **flac** (Vorbis Comments)
- **ogg / opus** (Vorbis Comments / Opus tags)
- **wma** (ASF)
- plus: wav, aiff, ape, wavpack, mpc, speex
- everything else (unknown extension) → left alone + reported

Recognized id tags (mapped to the template/override keys):

| Key | Meaning |
|---|---|
| `title` | Title — track name |
| `artist` | Artist — performer or recording artist |
| `album` | Album — album name |
| `albumartist` | Album artist — artist credited for the album |
| `band` | Band / group — band or group name |
| `year` / `date` | Year / date — release year or full release date |
| `track` | Track number — position on the album |
| `disc` | Disc number — position in a multi-disc set |
| `genre` | Genre |
| `composer` | Composer — songwriter or composer |
| `artists` | Contributing artists — featured or other performers |
| `comment` | Comment — notes or description |

…plus all other frames present in a file are **preserved untouched** when
`--update-meta` rewrites a tag.

## report at the end

Every run (dry run included) prints a line-oriented report following the
general pattern `{what happened}: {path}`:

```
moved: /out/Metallica/1986 - Master of Puppets/01 - Battery.mp3, source: /in/Metallica/…/Battery.mp3
copied: /out/…, source: /in/…                                (with --copy)
skipped (identical): /out/…, source: /in/…                   (copy mode: identical content already there)
deduplicated: /out/…, source: /in/…                           (move mode: redundant source removed, identical content kept at /out/…)
missing album: /in/Metallica/unknown/01.mp3                  (unprocessed: reason: path)
unsupported file type: /in/notes.xyz
```

- dry run prints the same lines it *would* print (`moved:`/`copied:`
  matching the selected mode), with `source:` pointing at the old path;
- an actual run prints the same, but with `--omit-success` only the
  error/unprocessed lines remain;
- after the line list: a summary with the **number** (not a list) of processed
  files, counts of skipped/deduplicated/removed-junk, and the full list of
  unprocessed files with reasons.

Exit codes:

| Code | Meaning |
|---|---|
| 0 | success — no errors, nothing left unprocessed |
| 1 | run completed but some files were unprocessed / per-file errors occurred |
| 2 | fatal error (bad arguments, unknown template variable, input missing, input/output overlap) |

## operational mode

1. **Plan** — build a full map of all incoming files and their intended
   destinations *before touching anything*: scan → read meta → normalize →
   apply overrides → render template → md5 → resolve duplicates/conflicts.
   The plan is deterministic (sorted by source path).
2. **Execute** — process the plan concurrently (tokio runtime, bounded by
   `--jobs`, cooperative, no locking of source folders). Per-file failures are
   fail-soft: reported, never abort the run, never leave a file half-moved
   (cross-device moves fall back to copy→verify→delete-source).
3. **Prune** (move mode only, never dry-run/copy) — once files are processed,
   delete now-empty input folders bottom-up: innermost first, then middle,
   then top; a folder containing only junk files (junk list above) is
   considered prunable. The bubble eventually reaches the input root, which is
   deleted too under the same rules. If leftovers remain (unprocessed files),
   those paths and every folder along the way to them remain in place.
4. **Report** + exit code.

Best case: all files (including album art) moved to output, input folder
completely gone, zero unprocessed, exit 0.

## important aspects

- **No files are lost** during processing (the only consolidation is identical
  input duplicates collapsing into the single output copy).
- Meta data is **not modified by default** — only with `--update-meta`, and
  then only for processed files.
- `--dry-run` never writes to disk.
- Best case: no errors, everything moved (incl. album art), input empty.

## external libraries

- prefer the **tokio** runtime for async (blocking FS work on
  `spawn_blocking`, bounded concurrency)
- prefer existing, actively maintained crates from crates.io with best
  long-term support (clap for CLI, lofty for tags, md-5 for checksums, …)
- no maintained crate exists for WMA/ASF tags → small in-house ASF
  read/write module (see execution-plan.md)