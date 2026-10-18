# musorter

A music collection sorter. Takes an input folder containing an arbitrary
structure of music files, reads each file's metadata (tags, with a
path/filename fallback), and **moves** the files into a deterministic,
template-driven folder structure. The same metadata always produces the same
output path — runs are stable and idempotent.

```
musorter <INPUT> -o <OUTPUT> [OPTIONS]
```

```
$ musorter ~/inbox -o ~/Music
moved: /Users/me/Music/Metallica/1986 - Master of Puppets/01 - Battery.mp3, source: /Users/me/inbox/Metallica/…/Battery.mp3
removed (junk): /Users/me/inbox/Metallica/…/CD.log
---
processed: 1
skipped (identical): 0
deduplicated: 0
junk removed: 1
unprocessed: 0
```

## Default output layout

```
{out}/{band}/{year - album}/{track - title}.{ext}
```

with every metadata value normalized before it becomes a path: title-cased,
trimmed, underscores removed, camelCase split, hyphens kept — so spelling
variants of the same album land in one folder:

```
Metallica/Master OF PUPPETS/1985  →  Metallica/1985 - Master Of Puppets
metallica/BlackAlbum              →  Metallica/Black Album
metallica/Kill-'Em-All/1982       →  Metallica/1982 - Kill-'Em-All
```

Missing optional parts collapse without artifacts (`year` missing →
`{album}/…`, `track` missing → `{title}.ext`). Missing **critical** fields
(`band`, `album`, `title` — when the template uses them) leave the file
untouched and reported; `--album "…"` (or any field override) unblocks it.

## Options

| Flag | Meaning |
|---|---|
| `-o, --out <DIR>` | output folder (required) |
| `-t, --template <TPL>` | custom layout, e.g. `{$band}/{$genre}/{$track - $title}.{$ext}` |
| `--dry-run` | print the plan; **never writes anything to disk** |
| `--copy` | copy instead of move (no folder pruning, sources untouched) |
| `--update-meta` | persist normalized values (incl. overrides) into **processed** files only |
| `--<field> <VALUE>` | global override for any field: `band artist albumartist album year date title track disc genre artists composer comment` |
| `--jobs <N>` | concurrency (default: CPU count) |
| `--omit-success` | print only failures/unprocessed lines |

Template variables: `$band $artist $albumartist $album $year $date $title
$track $disc $genre $artists $composer $comment $ext` (`$$` = literal `$`;
unknown variables are a fatal error). An **override always wins** over tags
and path inference; tags win over path inference.

## Conflict resolution & duplicates

Existing destination → md5 comparison: identical ⇒ skipped (the redundant
source is removed in move mode); different ⇒ `-1`, `-2`, … suffixes.
Byte-identical input duplicates mapping to the same destination collapse into
the single output copy — no files are ever lost. **md5 is only computed for
files involved in a destination conflict**; a clean run hashes nothing.

## Album art & other files

Images (`jpg jpeg png gif webp bmp`) move into their album directory keeping
their file name. Junk (`log cue m3u m3u8 pls sfv nfo diz txt url` and
`.DS_Store Thumbs.db desktop.ini`) never gets a destination; in move mode it
is removed while empty input folders are pruned bottom-up (bubbling all the
way to the input root when everything was processed). Unknown extensions are
left in place and reported.

## Formats

mp3, flac, m4a/mp4, ogg/opus, aac, wav, aiff, ape, wavpack, mpc, speex,
**wma/asf** (in-house ASF reader/writer — lofty doesn't support ASF).

## Exit codes

`0` — everything processed · `1` — some files unprocessed/failed ·
`2` — usage/fatal error (bad arguments, unknown variable, input/output overlap).

## Development

```bash
cargo test                 # 47 unit + 23 integration tests
cargo clippy --all-targets -- -D warnings
cargo fmt --check
./tests/gen_fixtures.sh    # regenerate tiny audio fixtures (needs ffmpeg; outputs committed)
```

See `SPEC.md` for the full specification and `execution-plan.md` for the
technical build plan (pipeline phases, conflict allocator, ASF format notes,
test strategy).