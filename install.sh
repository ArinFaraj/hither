#!/bin/sh
set -eu

REPO=ArinFaraj/hither
BIN_DIR="${HITHER_BIN_DIR:-$HOME/.local/bin}"

fail() {
  echo "hither install: $*" >&2
  exit 1
}

command -v curl >/dev/null || fail "curl is needed"
command -v tar >/dev/null || fail "tar is needed"

case "$(uname -s)" in
  Darwin) os=macos ;;
  Linux) os=linux ;;
  *) fail "there are no builds for $(uname -s) yet" ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) arch=arm64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) fail "there are no builds for $(uname -m) yet" ;;
esac

fetch() {
  curl -fsSL --proto '=https' --proto-redir '=https' -H "User-Agent: hither-install" "$@"
}

latest=$(fetch -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")
tag=${latest##*/}
case "$tag" in
  v[0-9]*) ;;
  *) fail "could not find the latest release" ;;
esac
version=${tag#v}
name="hither-$version-$os-$arch.tar.gz"
base="https://github.com/$REPO/releases/download/$tag"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "downloading hither $version for $os $arch"
fetch -o "$tmp/$name" "$base/$name"
fetch -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"

want=$(awk -v n="$name" '$2 == n || $2 == "*" n { print $1 }' "$tmp/SHA256SUMS")
[ -n "$want" ] || fail "SHA256SUMS does not list $name"
if command -v sha256sum >/dev/null; then
  got=$(sha256sum "$tmp/$name" | awk '{ print $1 }')
else
  got=$(shasum -a 256 "$tmp/$name" | awk '{ print $1 }')
fi
[ "$got" = "$want" ] || fail "$name does not match its checksum"

tar -xzf "$tmp/$name" -C "$tmp"
mkdir -p "$BIN_DIR"

if [ "$os" = macos ]; then
  apps="$HOME/Applications"
  mkdir -p "$apps"
  pkill -x HitherMenu 2>/dev/null || true
  rm -rf "$apps/Hither.app"
  ditto "$tmp/Hither.app" "$apps/Hither.app"
  ln -sf "$apps/Hither.app/Contents/MacOS/hither" "$BIN_DIR/hither"
  "$BIN_DIR/hither" autostart on
  open "$apps/Hither.app"
  echo "installed $apps/Hither.app, it is in the menu bar now"
else
  install -m 755 "$tmp/hither" "$BIN_DIR/hither.new"
  mv -f "$BIN_DIR/hither.new" "$BIN_DIR/hither"
  "$BIN_DIR/hither" autostart on || echo "start it yourself with: $BIN_DIR/hither daemon"
  echo "installed $BIN_DIR/hither"
fi

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "add $BIN_DIR to your PATH to run hither from a terminal" ;;
esac
echo "next: run 'hither pair' on one device and 'hither pair <ticket>' on the other"
