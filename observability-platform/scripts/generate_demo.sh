#!/usr/bin/env bash
set -euo pipefail

platform_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
repo_root=$(cd "$platform_root/.." && pwd)
demo_dir="$platform_root/docs/demo"
capture_dir=$(mktemp -d)
source_commit=${1:-$(git -C "$repo_root" rev-parse HEAD)}
trap 'rm -rf "$capture_dir"' EXIT

for command_name in node ffmpeg ffprobe git; do
  command -v "$command_name" >/dev/null 2>&1 || {
    echo "Missing required command: $command_name" >&2
    exit 1
  }
done

mkdir -p "$demo_dir"
node "$platform_root/browser-tests/capture.mjs" "$capture_dir"

ffmpeg -hide_banner -loglevel error -y -i "$capture_dir/capture.webm" \
  -vf "fps=24,scale=1440:900:flags=lanczos" \
  -c:v libx264 -crf 27 -preset slow -pix_fmt yuv420p -movflags +faststart \
  "$demo_dir/paper-control-plane.mp4"

ffmpeg -hide_banner -loglevel error -y -i "$capture_dir/capture.webm" \
  -filter_complex "[0:v]fps=6,scale=720:450:flags=lanczos,split[a][b];[a]palettegen=max_colors=96:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=3:diff_mode=rectangle" \
  -loop 0 "$demo_dir/paper-control-plane.gif"

install -m 0644 "$capture_dir/poster.png" "$demo_dir/poster.png"
node "$platform_root/scripts/build-demo-receipt.mjs" --source-commit "$source_commit"
node "$platform_root/scripts/verify-demo-media.mjs"

printf 'DEMO_MEDIA_OK source=%s gif=%s mp4=%s poster=%s receipt=%s\n' \
  "$source_commit" \
  "$demo_dir/paper-control-plane.gif" \
  "$demo_dir/paper-control-plane.mp4" \
  "$demo_dir/poster.png" \
  "$demo_dir/capture.json"
