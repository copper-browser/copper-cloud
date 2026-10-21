#!/usr/bin/env bash
# Canvas App lives in copper-browser/canvas-app. copper-cloud vendors its web build,
# dist/web/index.html, at the commit pinned in crates/copper-cloud/assets/CANVAS_APP_REF, as
# crates/copper-cloud/assets/canvas-app.html, which the server serves at /canvas/.
#   packaging/sync-canvas-app.sh               re-fetch the pinned build (e.g. after editing CANVAS_APP_REF)
#   packaging/sync-canvas-app.sh REF           pin REF (a canvas-app commit, tag or branch, resolved to a commit) and fetch it
#   packaging/sync-canvas-app.sh --check       exit 1 if canvas-app.html is not the pinned build
#   packaging/sync-canvas-app.sh --from FILE   vendor a local build (canvas-app: bun run build) — for trying changes; pin before merging
set -euo pipefail
cd "$(dirname "$0")/../crates/copper-cloud/assets"
repo=copper-browser/canvas-app
pin=CANVAS_APP_REF
dest=canvas-app.html
fetch() {
  curl -fsSL "https://raw.githubusercontent.com/$repo/$1/dist/web/index.html" -o "$2"
  grep -q 'id="web-root"' "$2" || { echo "sync: $1 has no Canvas App web build" >&2; exit 2; }
}
tmp=$(mktemp); trap 'rm -f "$tmp"' EXIT
case "${1:-}" in
  --check)
    fetch "$(cat "$pin")" "$tmp"
    cmp -s "$tmp" "$dest" && echo "canvas-app.html matches canvas-app $(cat "$pin")" || { echo "canvas-app.html differs from canvas-app $(cat "$pin"): run packaging/sync-canvas-app.sh" >&2; exit 1; }
    ;;
  --from)
    cp "$2" "$dest"; echo "vendored $2 (not pinned: run packaging/sync-canvas-app.sh REF before merging)"
    ;;
  "")
    fetch "$(cat "$pin")" "$dest"; echo "vendored canvas-app $(cat "$pin")"
    ;;
  *)
    sha=$(gh api "repos/$repo/commits/$1" --jq .sha 2>/dev/null || git ls-remote "https://github.com/$repo.git" "$1" | cut -f1)
    [[ -n "$sha" ]] || { echo "sync: no canvas-app ref $1" >&2; exit 2; }
    fetch "$sha" "$dest"; echo "$sha" > "$pin"; echo "pinned canvas-app $sha"
    ;;
esac
