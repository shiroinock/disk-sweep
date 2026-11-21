#!/bin/sh
# disk-sweep installer. Non-interactive, no sudo, safe to re-run.
#
#   curl -fsSL https://raw.githubusercontent.com/shiroinock/disk-sweep/main/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --setup      # also install the skill + MCP for detected agents
#
# Options (or environment variables):
#   --dir DIR        install directory            (DISK_SWEEP_INSTALL_DIR, default ~/.local/bin)
#   --version TAG    release tag such as v0.1.0   (DISK_SWEEP_VERSION, default latest)
#   --setup          run `disk-sweep setup --yes` afterwards
#   --from-source    build with cargo instead of downloading a release
#
# The last line of output is a single JSON object describing the result.

set -eu

REPO="shiroinock/disk-sweep"
DIR="${DISK_SWEEP_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${DISK_SWEEP_VERSION:-latest}"
SETUP=0
FROM_SOURCE=0

while [ $# -gt 0 ]; do
  case "$1" in
    --dir) DIR="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --setup) SETUP=1; shift ;;
    --from-source) FROM_SOURCE=1; shift ;;
    -h|--help) sed -n '2,15p' "$0" 2>/dev/null || true; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

say() { printf 'disk-sweep-install: %s\n' "$*" >&2; }
result() { printf '{"ok":%s,"method":"%s","path":"%s","version":"%s","on_path":%s,"message":"%s"}\n' "$1" "$2" "$3" "$4" "$5" "$6"; }
fail() { result false "${METHOD:-none}" "" "" false "$1"; exit 1; }

target() {
  os=$(uname -s); arch=$(uname -m)
  case "$arch" in
    arm64|aarch64) arch=aarch64 ;;
    x86_64|amd64) arch=x86_64 ;;
    *) return 1 ;;
  esac
  case "$os" in
    Darwin) echo "$arch-apple-darwin" ;;
    Linux) echo "$arch-unknown-linux-gnu" ;;
    *) return 1 ;;
  esac
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

download() {
  t=$(target) || return 1
  if [ -n "${DISK_SWEEP_BASE_URL:-}" ]; then base="$DISK_SWEEP_BASE_URL"   # mirrors and tests
  elif [ "$VERSION" = latest ]; then base="https://github.com/$REPO/releases/latest/download"
  else base="https://github.com/$REPO/releases/download/$VERSION"; fi
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  asset="disk-sweep-$t.tar.gz"
  say "downloading $base/$asset"
  curl -fsSL "$base/$asset" -o "$tmp/$asset" || return 1
  curl -fsSL "$base/$asset.sha256" -o "$tmp/$asset.sha256" || return 1
  want=$(cut -d' ' -f1 < "$tmp/$asset.sha256")
  got=$(sha256 "$tmp/$asset")
  [ "$want" = "$got" ] || fail "checksum mismatch for $asset"
  tar -xzf "$tmp/$asset" -C "$tmp"
  mkdir -p "$DIR"
  install -m 0755 "$tmp/disk-sweep" "$DIR/disk-sweep"
}

build() {
  command -v cargo >/dev/null 2>&1 || return 1
  say "building from source with cargo (this takes a few minutes)"
  cargo install --locked --git "https://github.com/$REPO" disk-sweep --root "$(dirname "$DIR")" >&2
}

METHOD=release
if [ "$FROM_SOURCE" = 1 ] || ! download; then
  METHOD=cargo
  build || fail "no prebuilt binary for this platform and cargo is not installed (install Rust from https://rustup.rs and re-run)"
fi

BIN="$DIR/disk-sweep"
[ -x "$BIN" ] || BIN="$(dirname "$DIR")/bin/disk-sweep"
[ -x "$BIN" ] || fail "install finished but $BIN is missing"
VER=$("$BIN" --version | cut -d' ' -f2)

ON_PATH=false
case ":$PATH:" in *":$(dirname "$BIN"):"*) ON_PATH=true ;; esac
MSG="installed"
[ "$ON_PATH" = true ] || MSG="installed; add $(dirname "$BIN") to PATH"

if [ "$SETUP" = 1 ]; then
  "$BIN" setup --yes --human >&2 || MSG="$MSG; setup reported errors"
  MSG="$MSG; setup done"
fi

say "$MSG"
result true "$METHOD" "$BIN" "$VER" "$ON_PATH" "$MSG"
