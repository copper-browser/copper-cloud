#!/usr/bin/env bash
# Vendor Canvas App (copper-browser/canvas-app, dist/web/index.html) as
# crates/copper-cloud/assets/canvas-app.html, which the server serves at /canvas/.
#   packaging/sync-canvas-app.sh [REF]         fetch REF (default main) from GitHub
#   packaging/sync-canvas-app.sh --from FILE   copy a local build
#   packaging/sync-canvas-app.sh --check [REF] exit 1 if the vendored copy differs
set -euo pipefail
cd "$(dirname "$0")/.."
dest=crates/copper-cloud/assets/canvas-app.html
check=0
if [[ "${1:-}" == --check ]]; then check=1; shift; fi
tmp=$(mktemp); trap 'rm -f "$tmp"' EXIT
if [[ "${1:-}" == --from ]]; then
  cp "$2" "$tmp"
else
  ref=${1:-main}
  curl -fsSL "https://raw.githubusercontent.com/copper-browser/canvas-app/$ref/dist/web/index.html" -o "$tmp"
fi
grep -q 'id="web-root"' "$tmp" || { echo "not a Canvas App build" >&2; exit 2; }
if (( check )); then
  cmp -s "$tmp" "$dest" && echo "canvas-app.html is up to date" || { echo "canvas-app.html differs from ${ref:-the given file}" >&2; exit 1; }
else
  cp "$tmp" "$dest" && echo "vendored $(wc -c <"$dest") bytes into $dest"
fi
