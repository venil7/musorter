#!/usr/bin/env bash
# Generates the tiny audio fixtures used by the integration tests.
# Dev-time only: outputs are committed so tests never need ffmpeg.
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p fixtures/untagged fixtures/tagged

SRC=(-f lavfi -i "sine=frequency=440:duration=0.3")
META=(-metadata title="some song" -metadata artist="some artist" \
      -metadata album="some album" -metadata album_artist="some artist" \
      -metadata track=1 -metadata date=1980 -metadata genre="rock" \
      -metadata comment="a comment")

enc() { # $1 = output path, rest = encoder args
  local out="$1"; shift
  ffmpeg -hide_banner -loglevel error -y "${SRC[@]}" "$@" -write_xing 1 "$out" 2>/dev/null \
    || ffmpeg -hide_banner -loglevel error -y "${SRC[@]}" "$@" "$out"
}

# --- untagged (metadata-fallback tests) ---
enc fixtures/untagged/song.mp3   -c:a libmp3lame -q:a 6
enc fixtures/untagged/song.flac  -c:a flac
enc fixtures/untagged/song.m4a   -c:a aac -b:a 32k
enc fixtures/untagged/song.ogg   -c:a vorbis -q:a 0 -strict -2 -ac 2
enc fixtures/untagged/song.opus  -c:a libopus -b:a 32k
enc fixtures/untagged/song.aac   -c:a aac -b:a 32k -f adts
enc fixtures/untagged/song.wma   -c:a wmav2 -b:a 32k -f asf
enc fixtures/untagged/song.wav   -c:a pcm_s16le
enc fixtures/untagged/song.aiff  -c:a pcm_s16le -f aiff

# --- tagged (known values, read-back tests) ---
enc fixtures/tagged/song.mp3  -c:a libmp3lame -q:a 6 "${META[@]}"
enc fixtures/tagged/song.flac -c:a flac        "${META[@]}"
enc fixtures/tagged/song.m4a  -c:a aac -b:a 32k "${META[@]}"
enc fixtures/tagged/song.wma  -c:a wmav2 -b:a 32k -f asf \
    -metadata title="some song" -metadata artist="some artist" \
    -metadata album="some album" -metadata track=1 -metadata date=1980 \
    -metadata genre="rock"

ls -la fixtures/untagged fixtures/tagged