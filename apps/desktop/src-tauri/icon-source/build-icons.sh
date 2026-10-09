#!/usr/bin/env bash
# Rebuilds the application's icons from icon.svg: the app's own blue "wind"
# glyph on Apple's 1024 px icon grid (824 px of content, centred, shadowed).
#
#   bash apps/desktop/src-tauri/icon-source/build-icons.sh
#
# macOS only: renders with Quick Look (qlmanage), which flattens onto white,
# so the SVG is rendered over white and over black and the alpha recovered
# from the difference. Needs Python 3 with Pillow.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
icons="$here/../icons"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cp "$here/icon.svg" "$work/white.svg"
sed 's|<g filter="url(#shadow)">|<rect width="1024" height="1024" fill="#000000"/><g filter="url(#shadow)">|' \
  "$here/icon.svg" > "$work/black.svg"
qlmanage -t -s 1024 -o "$work" "$work/white.svg" "$work/black.svg" >/dev/null 2>&1

python3 - "$work" <<'PY'
import sys
from PIL import Image

work = sys.argv[1]
white = Image.open(f"{work}/white.svg.png").convert("RGB").load()
black_image = Image.open(f"{work}/black.svg.png").convert("RGB")
black = black_image.load()
out = Image.new("RGBA", black_image.size)
pixels = out.load()
for y in range(black_image.size[1]):
    for x in range(black_image.size[0]):
        (rw, gw, bw), (rb, gb, bb) = white[x, y], black[x, y]
        alpha = max(0, min(255, 255 - max(rw - rb, gw - gb, bw - bb)))
        if alpha == 0:
            pixels[x, y] = (0, 0, 0, 0)
        else:
            pixels[x, y] = tuple(
                max(0, min(255, round(c * 255 / alpha))) for c in (rb, gb, bb)
            ) + (alpha,)
out.save(f"{work}/icon-1024.png")
PY

(cd "$here/../.." && pnpm tauri icon "$work/icon-1024.png" -o "$work/out" >/dev/null)
# Only the files the bundle already uses: no Android or iOS sets.
for file in "$icons"/*; do
  cp "$work/out/$(basename "$file")" "$file"
done
echo "Icons rebuilt in $icons"
