//! End-to-end CLI tests: every SPEC behavior exercised against real
//! filesystem trees and real audio fixtures (generated once via
//! `tests/gen_fixtures.sh`, committed).

use assert_cmd::Command;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;
use tempfile::TempDir;

const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture(kind: &str, name: &str) -> PathBuf {
    PathBuf::from(FIX).join(kind).join(name)
}

fn musorter(args: &[&str]) -> Output {
    Command::cargo_bin("musorter")
        .unwrap()
        .args(args)
        .output()
        .unwrap()
}

/// Run with the hashing counter exposed on stderr (`hashed=N`).
fn musorter_hashed(args: &[&str]) -> usize {
    let out = Command::cargo_bin("musorter")
        .unwrap()
        .args(args)
        .env("MUSORTER_DEBUG_STATS", "1")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    err.lines()
        .find_map(|l| l.split("hashed=").nth(1))
        .map(|n| n.trim().parse().unwrap_or(usize::MAX))
        .unwrap_or(usize::MAX)
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn mkdir(p: &Path) {
    std::fs::create_dir_all(p).unwrap();
}

fn place(fixture_path: &Path, dst: &Path) {
    mkdir(dst.parent().unwrap());
    std::fs::copy(fixture_path, dst).unwrap();
}

/// Recursive snapshot: relative path → (kind marker, len, mtime nanos).
fn snapshot(root: &Path) -> BTreeMap<String, (u8, u64, u128)> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, (u8, u64, u128)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            let md = std::fs::symlink_metadata(&p).unwrap();
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            if md.is_dir() {
                out.insert(rel, (0, 0, mtime));
                walk(base, &p, out);
            } else {
                out.insert(rel, (1, md.len(), mtime));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn exists(p: &Path) -> bool {
    p.exists()
}

// ---------------------------------------------------------------------------
// 1. dry-run: zero writes, full plan reported
// ---------------------------------------------------------------------------

#[test]
fn dry_run_writes_nothing_and_reports_the_plan() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    place(
        &fixture("untagged", "song.mp3"),
        &input.join("Band/Album/01 - my song.mp3"),
    );

    let before_in = snapshot(&input);
    let o = musorter(&[
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--dry-run",
    ]);

    assert_eq!(code(&o), 0, "stderr: {}", stderr(&o));
    let s = stdout(&o);
    let expect_dst = out.join("Band/Album/01 - My Song.mp3");
    assert!(
        s.contains(&format!("moved: {}, source: ", expect_dst.display())),
        "report missing planned line:\n{s}"
    );
    assert!(s.contains("processed: 1"), "summary missing:\n{s}");

    // NOTHING may have changed on disk — input identical, output not created.
    assert_eq!(snapshot(&input), before_in, "dry-run touched the input!");
    assert!(!out.exists(), "dry-run created the output folder!");
}

// ---------------------------------------------------------------------------
// 2. move: exact output tree from tags, input fully consumed + pruned
// ---------------------------------------------------------------------------

#[test]
fn move_tagged_mp3_into_exact_tree_and_prune_input() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    place(&fixture("tagged", "song.mp3"), &input.join("song.mp3"));

    let o = musorter(&[input.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "stderr: {}", stderr(&o));

    let dst = out.join("Some Artist/1980 - Some Album/01 - Some Song.mp3");
    assert!(exists(&dst), "missing {dst:?}\n{}", stdout(&o));
    assert!(stdout(&o).contains(&format!("moved: {}", dst.display())));

    // best-case scenario: input root completely gone (SPEC)
    assert!(!input.exists(), "input root was not pruned");
    assert!(stdout(&o).contains("unprocessed: 0"));
}

// ---------------------------------------------------------------------------
// 3. metadata fallback from path + normalization of folder names
// ---------------------------------------------------------------------------

#[test]
fn path_fallback_normalizes_folders_and_filename() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    place(
        &fixture("untagged", "song.mp3"),
        &input.join("metallica/BlackAlbum/01 - sad but true.mp3"),
    );

    let o = musorter(&[input.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "stderr: {}", stderr(&o));
    assert!(exists(
        &out.join("Metallica/Black Album/01 - Sad But True.mp3")
    ));
    assert!(!input.exists());
}

// ---------------------------------------------------------------------------
// 4. missing critical data: left alone + reported; --album unblocks
// ---------------------------------------------------------------------------

#[test]
fn missing_album_leaves_file_alone_and_override_unblocks() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    let file = input.join("Metallica/song.mp3"); // single dir ⇒ no album
    place(&fixture("untagged", "song.mp3"), &file);

    let o = musorter(&[input.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert_eq!(code(&o), 1, "expected exit 1, stderr: {}", stderr(&o));
    let s = stdout(&o);
    assert!(
        s.contains(&format!("missing album: {}", file.display())),
        "reason line missing:\n{s}"
    );
    assert!(exists(&file), "file must be left alone");
    assert!(!out.exists(), "nothing should be written");

    // Now the override unblocks it (SPEC: parameter takes precedence).
    let o = musorter(&[
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--album",
        "Kill 'Em All",
    ]);
    assert_eq!(code(&o), 0, "stderr: {}", stderr(&o));
    assert!(exists(&out.join("Metallica/Kill 'Em All/Song.mp3")));
    assert!(!input.exists());
}

// ---------------------------------------------------------------------------
// 5+6. destination conflicts: suffix on different content, skip on identical
// ---------------------------------------------------------------------------

#[test]
fn existing_different_destination_gets_numeric_suffix() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    place(&fixture("tagged", "song.mp3"), &input.join("song.mp3"));

    // Same SIZE as the incoming file, different bytes → forces the md5 path.
    let dest = out.join("Some Artist/1980 - Some Album/01 - Some Song.mp3");
    place(&fixture("tagged", "song.mp3"), &dest);
    let mut bytes = std::fs::read(&dest).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&dest, bytes).unwrap();

    let o = musorter(&[input.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "stderr: {}", stderr(&o));
    assert!(exists(&dest), "pre-existing file must be untouched");
    let suffixed = out.join("Some Artist/1980 - Some Album/01 - Some Song-1.mp3");
    assert!(exists(&suffixed), "missing {suffixed:?}\n{}", stdout(&o));
    assert!(stdout(&o).contains("-1.mp3"));
}

#[test]
fn existing_identical_destination_is_skipped_and_source_removed() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    place(&fixture("tagged", "song.mp3"), &input.join("song.mp3"));

    let dest = out.join("Some Artist/1980 - Some Album/01 - Some Song.mp3");
    place(&fixture("tagged", "song.mp3"), &dest);

    let o = musorter(&[input.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "stderr: {}", stderr(&o));
    let s = stdout(&o);
    assert!(
        s.contains(&format!(
            "deduplicated: {}, source: {}",
            dest.display(),
            input.join("song.mp3").display()
        )),
        "skip line missing:\n{s}"
    );
    assert!(
        !exists(&input.join("song.mp3")),
        "redundant source not removed"
    );
    assert!(exists(&dest));
}

// ---------------------------------------------------------------------------
// 7. input duplicates collapse into one output copy
// ---------------------------------------------------------------------------

#[test]
fn input_duplicates_collapse_to_single_output() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    place(&fixture("tagged", "song.mp3"), &input.join("one/song.mp3"));
    place(&fixture("tagged", "song.mp3"), &input.join("two/song.mp3"));

    let o = musorter(&[input.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "stderr: {}", stderr(&o));
    let s = stdout(&o);
    assert!(s.contains("moved: "), "\n{s}");
    assert!(s.contains("deduplicated: "), "\n{s}");

    let dst = out.join("Some Artist/1980 - Some Album/01 - Some Song.mp3");
    assert!(exists(&dst));
    // exactly one copy in output, input fully gone (SPEC no-loss clause)
    assert!(!input.exists());
    let copies = count_files_with_name(&out, "01 - Some Song.mp3");
    assert_eq!(copies, 1, "expected exactly one output copy, got {copies}");
}

fn count_files_with_name(root: &Path, name: &str) -> usize {
    fn walk(dir: &Path, name: &str, n: &mut usize) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, name, n);
                } else if p.file_name().and_then(|f| f.to_str()) == Some(name) {
                    *n += 1;
                }
            }
        }
    }
    let mut n = 0;
    walk(root, name, &mut n);
    n
}

// ---------------------------------------------------------------------------
// 8. copy mode: sources untouched, no pruning
// ---------------------------------------------------------------------------

#[test]
fn copy_mode_keeps_source_tree_and_prunes_nothing() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    let src = input.join("someplace/song.mp3");
    place(&fixture("tagged", "song.mp3"), &src);
    mkdir(&input.join("someplace"));
    std::fs::write(input.join("someplace/notes.log"), b"log").unwrap();

    let o = musorter(&[
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--copy",
    ]);
    assert_eq!(code(&o), 0, "stderr: {}", stderr(&o));
    let s = stdout(&o);
    assert!(s.contains("copied: "), "\n{s}");
    assert!(s.contains("ignored (junk): "), "\n{s}");

    assert!(exists(&src), "copy mode must not touch sources");
    assert!(exists(&input.join("someplace/notes.log")), "junk must stay");
    assert!(exists(&input.join("someplace")), "copy mode must not prune");
    assert!(exists(
        &out.join("Some Artist/1980 - Some Album/01 - Some Song.mp3")
    ));
}

// ---------------------------------------------------------------------------
// 9. --update-meta: overrides persisted, unprocessed files untouched
// ---------------------------------------------------------------------------

#[test]
fn update_meta_persists_override_and_never_touches_unprocessed() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    place(&fixture("tagged", "song.mp3"), &input.join("song.mp3"));
    // album cannot be resolved → stays unprocessed
    let untouched = input.join("Metallica/orphan.mp3");
    place(&fixture("untagged", "song.mp3"), &untouched);
    let before = musorter_hash(&untouched);

    let o = musorter(&[
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--year",
        "1990",
        "--update-meta",
    ]);
    assert_eq!(code(&o), 1, "stderr: {}", stderr(&o)); // orphan ⇒ 1
    let s = stdout(&o);
    assert!(s.contains("missing album: "), "\n{s}");

    // destination path reflects the override
    let dst = out.join("Some Artist/1990 - Some Album/01 - Some Song.mp3");
    assert!(exists(&dst), "missing {dst:?}\n{s}");

    // tag content on the destination: override + normalization persisted
    let tags = musorter::meta::tags::read_tags(&dst).unwrap();
    assert_eq!(
        tags.get(&musorter::model::MetaKey::Year)
            .map(String::as_str),
        Some("1990")
    );
    assert_eq!(
        tags.get(&musorter::model::MetaKey::Artist)
            .map(String::as_str),
        Some("Some Artist")
    );
    assert_eq!(
        tags.get(&musorter::model::MetaKey::Genre)
            .map(String::as_str),
        Some("Rock")
    );

    // unprocessed file: byte-identical (never tag-written, never moved)
    assert!(exists(&untouched));
    assert_eq!(
        musorter_hash(&untouched),
        before,
        "unprocessed file was modified!"
    );
}

fn musorter_hash(p: &Path) -> String {
    musorter::hash::md5_blocking(p).unwrap()
}

// ---------------------------------------------------------------------------
// 10. album art follows its album
// ---------------------------------------------------------------------------

#[test]
fn album_art_moves_into_the_album_directory() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    place(
        &fixture("untagged", "song.mp3"),
        &input.join("Band/Album/01 - t.mp3"),
    );
    mkdir(&input.join("Band/Album"));
    std::fs::write(input.join("Band/Album/cover.jpg"), b"\xFF\xD8fakejpeg").unwrap();
    std::fs::write(input.join("Band/Album/folder.JPG"), b"\xFF\xD8fakejpeg2").unwrap();

    let o = musorter(&[input.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "stderr: {}", stderr(&o));
    assert!(exists(&out.join("Band/Album/01 - T.mp3")));
    assert!(exists(&out.join("Band/Album/cover.jpg")), "cover not moved");
    assert!(
        exists(&out.join("Band/Album/folder.JPG")),
        "folder art not moved"
    );
    assert!(!input.exists(), "input not fully consumed");
}

// ---------------------------------------------------------------------------
// 11. junk: removed while pruning (move), kept (copy)
// ---------------------------------------------------------------------------

#[test]
fn junk_removed_in_move_mode_and_kept_in_copy_mode() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    place(
        &fixture("untagged", "song.mp3"),
        &input.join("Band/Album/01 - t.mp3"),
    );
    std::fs::write(input.join("Band/Album/cd.log"), b"log").unwrap();
    std::fs::write(input.join("Band/Album/track.cue"), b"cue").unwrap();

    let o = musorter(&[input.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "stderr: {}", stderr(&o));
    let s = stdout(&o);
    assert!(s.contains("removed (junk): "), "\n{s}");
    assert!(!input.exists(), "junk-only folders must be pruned");

    // copy mode: junk stays, reported as ignored
    let input2 = root.join("in2");
    let out2 = root.join("out2");
    place(
        &fixture("untagged", "song.mp3"),
        &input2.join("Band/Album/01 - t.mp3"),
    );
    std::fs::write(input2.join("Band/Album/cd.log"), b"log").unwrap();
    let o = musorter(&[
        input2.to_str().unwrap(),
        "-o",
        out2.to_str().unwrap(),
        "--copy",
    ]);
    assert_eq!(code(&o), 0, "stderr: {}", stderr(&o));
    assert!(stdout(&o).contains("ignored (junk): "));
    assert!(exists(&input2.join("Band/Album/cd.log")));
}

// ---------------------------------------------------------------------------
// 12. unsupported files: reported, keep their ancestor chain alive
// ---------------------------------------------------------------------------

#[test]
fn unsupported_file_reported_and_keeps_folder_chain() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    place(
        &fixture("untagged", "song.mp3"),
        &input.join("Band/Album/01 - t.mp3"),
    );
    let junk = input.join("Band/Album/program.xyz");
    std::fs::write(&junk, b"binary").unwrap();

    let o = musorter(&[input.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert_eq!(code(&o), 1, "stderr: {}", stderr(&o));
    let s = stdout(&o);
    assert!(
        s.contains(&format!("unsupported file type: {}", junk.display())),
        "\n{s}"
    );
    assert!(exists(&out.join("Band/Album/01 - T.mp3")));
    assert!(exists(&junk), "unsupported file must be left alone");
    assert!(
        exists(&input.join("Band/Album")),
        "ancestors of leftovers stay"
    );
    assert!(exists(&input.join("Band")));
}

// ---------------------------------------------------------------------------
// 13+14. WMA: our in-house ASF reader/writer end-to-end
// ---------------------------------------------------------------------------

#[test]
fn wma_tags_read_and_moved_to_tag_derived_path() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    place(&fixture("tagged", "song.wma"), &input.join("song.wma"));

    let o = musorter(&[input.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "stderr: {}", stderr(&o));
    let dst = out.join("Some Artist/1980 - Some Album/01 - Some Song.wma");
    assert!(exists(&dst), "missing {dst:?}\n{}", stdout(&o));
    assert!(!input.exists());
}

#[test]
fn wma_update_meta_roundtrip_through_our_writer() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    place(
        &fixture("untagged", "song.wma"),
        &input.join("Band/Album/01 - song.wma"),
    );

    let o = musorter(&[
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--genre",
        "heavy metal",
        "--update-meta",
    ]);
    assert_eq!(code(&o), 0, "stderr: {}", stderr(&o));
    let dst = out.join("Band/Album/01 - Song.wma");
    assert!(exists(&dst), "missing {dst:?}\n{}", stdout(&o));

    let tags = musorter::meta::asf::read_tags(&dst).unwrap();
    assert_eq!(
        tags.get(&musorter::model::MetaKey::Genre)
            .map(String::as_str),
        Some("Heavy Metal")
    );
    assert_eq!(
        tags.get(&musorter::model::MetaKey::Title)
            .map(String::as_str),
        Some("Song")
    );
    assert_eq!(
        tags.get(&musorter::model::MetaKey::Album)
            .map(String::as_str),
        Some("Album")
    );
}

// ---------------------------------------------------------------------------
// 15. idempotency: a second run over sorted data is a no-op
// ---------------------------------------------------------------------------

#[test]
fn second_copy_run_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    place(&fixture("tagged", "song.mp3"), &input.join("song.mp3"));

    let args = [
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--copy",
    ];
    let o1 = musorter(&args);
    assert_eq!(code(&o1), 0, "stderr: {}", stderr(&o1));

    let o2 = musorter(&args);
    assert_eq!(code(&o2), 0, "stderr: {}", stderr(&o2));
    let s = stdout(&o2);
    assert!(
        s.contains("skipped (identical): "),
        "second run must skip:\n{s}"
    );
    assert!(
        s.contains("processed: 0"),
        "second run must process nothing:\n{s}"
    );
    assert!(exists(&input.join("song.mp3")), "copy mode keeps sources");
}

// ---------------------------------------------------------------------------
// 16+17. templates and usage errors → exit 2
// ---------------------------------------------------------------------------

#[test]
fn custom_template_renders_requested_fields() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    place(&fixture("tagged", "song.mp3"), &input.join("song.mp3"));

    let o = musorter(&[
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "-t",
        "{$band}/{$genre}/{$title}.{$ext}",
        "--genre",
        "rock",
    ]);
    assert_eq!(code(&o), 0, "stderr: {}", stderr(&o));
    assert!(
        exists(&out.join("Some Artist/Rock/Some Song.mp3")),
        "{}",
        stdout(&o)
    );
}

#[test]
fn unknown_template_variable_is_fatal_exit_2() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    mkdir(&input);
    let o = musorter(&[
        input.to_str().unwrap(),
        "-o",
        root.join("out").to_str().unwrap(),
        "-t",
        "{$band}/{$nope}.{$ext}",
    ]);
    assert_eq!(code(&o), 2);
    assert!(
        stderr(&o).contains("unknown template variable $nope"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn missing_out_argument_is_fatal_exit_2() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    mkdir(&input);
    let o = musorter(&[input.to_str().unwrap()]);
    assert_eq!(code(&o), 2);
}

#[test]
fn output_inside_input_is_fatal_exit_2() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    mkdir(&input);
    let o = musorter(&[
        input.to_str().unwrap(),
        "-o",
        input.join("sorted").to_str().unwrap(),
    ]);
    assert_eq!(code(&o), 2);
    assert!(stderr(&o).contains("must not be inside"), "{}", stderr(&o));
}

// ---------------------------------------------------------------------------
// 18. every supported container round-trips through the sorter
// ---------------------------------------------------------------------------

#[test]
fn all_supported_formats_are_processed() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    for ext in ["mp3", "flac", "m4a", "ogg", "opus", "aac", "wav", "aiff"] {
        place(
            &fixture("untagged", format!("song.{ext}").as_str()),
            &input.join(format!("Band/Album/01 - track.{ext}")),
        );
    }

    let o = musorter(&[input.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "stderr: {}", stderr(&o));
    for ext in ["mp3", "flac", "m4a", "ogg", "opus", "aac", "wav", "aiff"] {
        let dst = out.join(format!("Band/Album/01 - Track.{ext}"));
        assert!(
            exists(&dst),
            "format {ext} not processed: {dst:?}\n{}",
            stdout(&o)
        );
    }
    assert!(!input.exists(), "input must be fully consumed");
}

// ---------------------------------------------------------------------------
// 19. --omit-success prints only failures
// ---------------------------------------------------------------------------

#[test]
fn omit_success_prints_only_failures() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    place(&fixture("tagged", "song.mp3"), &input.join("song.mp3"));
    place(
        &fixture("untagged", "song.mp3"),
        &input.join("Metallica/orphan.mp3"),
    );

    let o = musorter(&[
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--omit-success",
    ]);
    assert_eq!(code(&o), 1, "stderr: {}", stderr(&o));
    let s = stdout(&o);
    // Only the report body (before the summary separator) is filtered.
    let body: Vec<&str> = s.lines().take_while(|l| *l != "---").collect();
    let any_success_line = body.iter().any(|l| {
        l.starts_with("moved: ")
            || l.starts_with("copied: ")
            || l.starts_with("skipped (identical): ")
            || l.starts_with("deduplicated: ")
            || l.starts_with("removed (junk): ")
    });
    assert!(!any_success_line, "success lines must be suppressed:\n{s}");
    assert!(s.contains("missing album: "), "\n{s}");
    // summary always printed
    assert!(s.contains("processed: 1"), "\n{s}");
}

// ---------------------------------------------------------------------------
// md5 is computed ONLY for destination conflicts (SPEC clarification)
// ---------------------------------------------------------------------------

#[test]
fn clean_run_never_hashes_and_conflict_run_does() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    place(&fixture("tagged", "song.mp3"), &input.join("song.mp3"));

    // Clean run: destination is free ⇒ the file must NOT be hashed (SPEC
    // clarification: md5 only for destination conflicts).
    let n = musorter_hashed(&[
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--dry-run",
    ]);
    assert_eq!(n, 0, "clean run must not compute any md5");

    // Conflict run: same-size different-content destination ⇒ md5 required.
    let dest = out.join("Some Artist/1980 - Some Album/01 - Some Song.mp3");
    place(&fixture("tagged", "song.mp3"), &dest);
    let mut bytes = std::fs::read(&dest).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&dest, bytes).unwrap();
    let n = musorter_hashed(&[
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--dry-run",
    ]);
    assert!(n >= 2, "conflict must hash both sides, hashed={n}");
}

// ---------------------------------------------------------------------------
// 20. symlinks are refused, reported, never followed
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn symlinks_are_reported_and_not_followed() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let input = root.join("in");
    let out = root.join("out");
    place(
        &fixture("untagged", "song.mp3"),
        &input.join("Band/Album/01 - t.mp3"),
    );
    std::os::unix::fs::symlink(input.join("Band"), input.join("link_to_band")).unwrap();

    let o = musorter(&[input.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert_eq!(code(&o), 1, "stderr: {}", stderr(&o));
    let s = stdout(&o);
    assert!(
        s.contains(&format!(
            "symlink: {}",
            input.join("link_to_band").display()
        )),
        "\n{s}"
    );
    assert!(
        std::fs::symlink_metadata(input.join("link_to_band")).is_ok(),
        "symlink entry must stay"
    );
    assert!(exists(&out.join("Band/Album/01 - T.mp3")));
}
