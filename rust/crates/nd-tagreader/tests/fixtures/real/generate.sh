#!/usr/bin/env bash
# Recreate small, synthetic, redistributable media. FFmpeg is generation-only;
# Rust tests read the committed files and never skip when ffmpeg is unavailable.
set -euo pipefail
cd "$(dirname "$0")"
for extension in mp3 flac m4a ogg; do
  case "$extension" in
    mp3) codec=(-c:a libmp3lame -b:a 128k -id3v2_version 4) ;;
    flac) codec=(-c:a flac -sample_fmt s16) ;;
    m4a) codec=(-c:a aac -b:a 96k) ;;
    ogg) codec=(-c:a vorbis -strict experimental -q:a 2) ;;
  esac
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i 'sine=frequency=440:sample_rate=44100:duration=0.4' -ac 2 \
    "${codec[@]}" \
    -metadata title='Real café' -metadata artist='Artist One' \
    -metadata album='Synthetic Album' -metadata album_artist='Album Artist' \
    -metadata genre='Rock' -metadata date='2024-03-02' \
    -metadata track='2/10' -metadata disc='1/2' \
    -metadata comment=$'Comment one\nComment two' \
    -metadata lyrics=$'[00:00.00]Real lyrics\n[00:00.20]Second line' \
    "known.$extension"
done
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i 'color=c=red:s=2x2' -frames:v 1 cover.png
