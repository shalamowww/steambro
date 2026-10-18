#!/usr/bin/env bash
# Builds a release of steambro for Windows from WSL: a portable zip and an installer.
#   scripts/release.sh           writes target/release/steambro-v<version>-windows-x64.zip
#                                and, with NSIS, steambro-v<version>-windows-x64-setup.exe
# Both hold steambro.exe, WebView2Loader.dll, and LICENSE.txt (GPL-3.0).
# Needs the mingw-w64 cross toolchain (sudo apt install mingw-w64); the installer needs NSIS
# (sudo apt install nsis), or MAKENSIS and NSISDIR pointing at an unpacked copy.
# Run it in a clean export of a commit (git archive <commit> | tar -x -C <folder>): the exe holds
# the ui folder as it is, so files that git doesn't track would go into it.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
target=x86_64-pc-windows-gnu
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml" | head -1)"
out="$repo/target/release"
name="steambro-v$version-windows-x64"

cargo build --quiet --release --manifest-path "$repo/Cargo.toml" --target $target

# With the GNU toolchain, WebView2's loader is a DLL that must sit next to the exe.
loader="$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -path '*webview2-com-sys-*/x64/WebView2Loader.dll' | sort -V | tail -1)"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
files="$stage/steambro"
mkdir "$files"
cp "$repo/target/$target/release/steambro.exe" "$loader" "$files/"
cp "$repo/LICENSE" "$files/LICENSE.txt"
mkdir -p "$out"
rm -f "$out/$name.zip"
python3 -c 'import shutil, sys; shutil.make_archive(sys.argv[1], "zip", sys.argv[2], "steambro")' "$out/$name" "$stage"
echo "$out/$name.zip"

makensis="${MAKENSIS:-$(command -v makensis || true)}"
if [[ -z "$makensis" ]]; then
  echo "no makensis, so no installer" >&2
  exit 0
fi
"$makensis" -V2 -DVERSION="$version" -DEXE="$files/steambro.exe" -DLOADER="$files/WebView2Loader.dll" \
  -DLICENSE="$files/LICENSE.txt" \
  -DICON="$repo/icons/icon.ico" -DOUT="$out/$name-setup.exe" "$repo/scripts/installer.nsi"
echo "$out/$name-setup.exe"
