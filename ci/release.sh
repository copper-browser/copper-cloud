#!/usr/bin/env bash
# Roadrunner build helper (.woodpecker/ci.yaml, .woodpecker/release.yaml); port of
# .github/workflows/release.yml.
#   bash ci/release.sh build vX.Y.Z    # host: dist/ from a rust container on the Dagger engine
#   GH_TOKEN=... bash ci/release.sh publish vX.Y.Z   # host: GitHub release from dist/
#   bash ci/release.sh dist vX.Y.Z     # in the container: tag check, portal, zigbuild, package
#   bash ci/release.sh portal          # in a rust container: pinned bun -> portal/out
# Tools are pinned by version + sha256 (dir: $RELEASE_TOOLS, see below).
set -euo pipefail

BUN_VERSION=1.4.4         # oven-sh/setup-bun@v2 default (latest) when ported
ZIG_VERSION=0.14.1        # mlugg/setup-zig version in release.yml
ZIGBUILD_VERSION=0.24.1   # `cargo install cargo-zigbuild` (latest) when ported
GH_VERSION=2.103.3
RUST_IMAGE=public.ecr.aws/docker/library/rust:1-bookworm
REPO=copper-browser/copper-cloud
NPM_REGISTRY=${NPM_REGISTRY:-https://registry.npmjs.org/}  # public tree: no internal hosts (tests/public_tree.rs)
# Release matrix: <rust target> <archive arch>.
MATRIX="x86_64-unknown-linux-gnu:x86_64 aarch64-unknown-linux-gnu:aarch64"

usage='usage: release.sh build|publish|dist vX.Y.Z | portal'
cmd=${1:?$usage}
tag=${2:-}
cd "$(dirname "$0")/.."
# Host commands keep tools in the checkout; container commands outside /src (tests scan the tree).
case "$cmd" in
  build | publish) tools=${RELEASE_TOOLS:-$PWD/.release-tools} ;;
  *) tools=${RELEASE_TOOLS:-/tmp/release-tools} ;;
esac
mkdir -p "$tools"

case "$(uname -m)" in
  x86_64)
    bun_arch=x64 gh_arch=amd64
    bun_sha=93ca5a2abe45f3bcac6dd55baff8bb06c4ac824b70f89650b54e5c8b80dbef18
    zig_sha=24aeeec8af16c381934a6cd7d95c807a8cb2cf7df9fa40d359aa884195c4716c
    gh_sha=4c806978ffce1cec70853302a56d70ac6da4f19bae7adca4e1a8ab00633dd91d
    ;;
  aarch64)
    bun_arch=aarch64 gh_arch=arm64
    bun_sha=ab95e7e692a53d25a1baa98ee395a17f5f264aedfb3cb15811432d54142f70eb
    zig_sha=f7a654acc967864f7a050ddacfaa778c7504a0eca8d2b678839c21eea47c992b
    gh_sha=ee32df2323c8847f5fac6cb6b4b69ca75828e9329a8f22f677021f7b3306ce63
    ;;
  *) echo "release.sh: unsupported machine $(uname -m)" >&2; exit 1 ;;
esac

# unpack <url> <sha256> <dir>: download into $tools, check the sha256, extract into $tools/<dir>.
unpack() {
  local file="$tools/${1##*/}"
  [ -f "$file" ] || curl -fsSL --retry 3 --connect-timeout 20 --max-time 600 -o "$file" "$1"
  echo "$2  $file" | sha256sum -c --quiet -
  rm -rf "${tools:?}/$3" && mkdir -p "$tools/$3"
  tar -xf "$file" -C "$tools/$3" --strip-components=1
}

need_tag() {
  [[ $tag == v* ]] || { echo "$usage (got tag '$tag')" >&2; exit 1; }
}

portal() {
  unpack "${NPM_REGISTRY%/}/@oven/bun-linux-$bun_arch/-/bun-linux-$bun_arch-$BUN_VERSION.tgz" "$bun_sha" bun
  export PATH="$tools/bun/bin:$PATH"
  bun --version
  (cd portal && NPM_CONFIG_REGISTRY=$NPM_REGISTRY bun install --frozen-lockfile && bun run build)
  test -f portal/out/index.html
}

# package <rust target> <arch>: the archive + .sha256 sidecar, exactly as release.yml.
package() {
  local name="copper-cloud-${version}-linux-$2" stage
  stage="$(mktemp -d)/$name"
  mkdir -p "$stage/packaging" dist
  cp "${CARGO_TARGET_DIR:-target}/$1/release/copper-cloud" "$stage/copper-cloud"
  cp install.sh "$stage/install.sh"
  cp packaging/copper-cloud.service "$stage/packaging/copper-cloud.service"
  cp README.md "$stage/README.md"
  cp LICENSE "$stage/LICENSE"
  chmod 0755 "$stage/copper-cloud" "$stage/install.sh"
  file "$stage/copper-cloud"
  # Files at the archive root: `tar xzf … && sudo ./install.sh`.
  tar -C "$stage" -czf "dist/$name.tar.gz" copper-cloud install.sh packaging README.md LICENSE
  (cd dist && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
  cat "dist/$name.tar.gz.sha256"
}

# In the rust container (or any Linux box with rustup): everything release.yml's build job did.
dist() {
  need_tag
  rustup toolchain install --no-self-update 2>/dev/null || rustup show active-toolchain
  version=$(cargo pkgid -p copper-cloud)
  version=${version##*[#@]}
  [ "$version" = "${tag#v}" ] || { echo "tag $tag != crate version $version" >&2; exit 1; }
  portal
  unpack "https://ziglang.org/download/$ZIG_VERSION/zig-$(uname -m)-linux-$ZIG_VERSION.tar.xz" "$zig_sha" zig
  cargo install --locked -j4 --root "$tools/zigbuild" "cargo-zigbuild@$ZIGBUILD_VERSION"
  export PATH="$tools/zig:$tools/zigbuild/bin:$PATH"
  rm -rf dist
  for m in $MATRIX; do
    rustup target add "${m%%:*}"
    cargo zigbuild --release --locked -j4 -p copper-cloud --target "${m%%:*}.2.28"
    package "${m%%:*}" "${m##*:}"
  done
  (cd dist && cat ./*.sha256 > SHA256SUMS)
  ls -l dist
}

# Host (engine VM): run dist() in a rust container through the persistent Dagger engine.
build() {
  need_tag
  rm -rf dist
  export DO_NOT_TRACK=1 DAGGER_NO_NAG=1 DAGGER_NO_UPDATE_CHECK=1
  export _EXPERIMENTAL_DAGGER_RUNNER_HOST=${_EXPERIMENTAL_DAGGER_RUNNER_HOST:-unix:///run/dagger-engine/engine.sock}
  timeout 5400 dagger --progress=plain core container from --address="$RUST_IMAGE" \
    with-mounted-cache --path=/usr/local/cargo/registry --cache=copper-cloud-cargo-registry \
    with-directory --path=/src --source=. --exclude=.git,target,dist,.release-tools,portal/node_modules \
    with-workdir --path=/src \
    with-env-variable --name=COPPER_CLOUD_GIT_SHA --value="${CI_COMMIT_SHA:-unknown}" \
    with-env-variable --name=CARGO_TERM_COLOR --value=never \
    with-exec --args=bash,ci/release.sh,dist,"$tag" \
    directory --path=/src/dist export --path=./dist
  ls -l dist
  cat dist/SHA256SUMS
}

# Host: upload dist/* (2 archives, 2 .sha256, SHA256SUMS) like softprops/action-gh-release
# with generate_release_notes; a re-run replaces the assets of an existing release.
publish() {
  need_tag
  : "${GH_TOKEN:?GH_TOKEN must be set}"
  if [ "$(find dist -name '*.tar.gz' | wc -l)" != 2 ] || [ ! -s dist/SHA256SUMS ]; then
    echo "dist/ incomplete" >&2
    exit 1
  fi
  unpack "https://github.com/cli/cli/releases/download/v$GH_VERSION/gh_${GH_VERSION}_linux_$gh_arch.tar.gz" "$gh_sha" gh
  export GH_CONFIG_DIR="$tools/gh-config" GH_PROMPT_DISABLED=1 GH_NO_UPDATE_NOTIFIER=1
  local gh="$tools/gh/bin/gh"
  if timeout 60 "$gh" release view "$tag" --repo "$REPO" >/dev/null 2>&1; then
    timeout 900 "$gh" release upload "$tag" dist/* --repo "$REPO" --clobber
  else
    timeout 900 "$gh" release create "$tag" dist/* --repo "$REPO" --verify-tag --title "$tag" --generate-notes
  fi
}

case "$cmd" in
  build | publish | dist | portal) "$cmd" ;;
  *) echo "$usage" >&2; exit 1 ;;
esac
