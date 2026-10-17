#!/usr/bin/env bash
# Builds steambro for Windows from WSL and runs it there.
#   scripts/win.sh               start the tray app (blocks until it quits)
#   scripts/win.sh list -v       run a command
#   scripts/win.sh --test        run the unit tests on Windows
# Needs the mingw-w64 cross toolchain: sudo apt install mingw-w64
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
target=x86_64-pc-windows-gnu

# Steam will launch steambro too, so the dev copy lives on the Windows drive, not the WSL share.
local_app_data="$(cmd.exe /c 'echo %LOCALAPPDATA%' 2>/dev/null | tr -d '\r')"
dest="$(wslpath "$local_app_data")/steambro-dev"
mkdir -p "$dest"

# With the GNU toolchain, WebView2's loader is a DLL next to the exe; MSVC builds link it in.
loader="$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -path '*webview2-com-sys-*/x64/WebView2Loader.dll' | sort -V | tail -1)"
cmp -s "$loader" "$dest/WebView2Loader.dll" || cp "$loader" "$dest/"

# Pass WebView2 options through to Windows, e.g. WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222
export WSLENV="${WSLENV:+$WSLENV:}WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS"

if [[ "${1:-}" == "--test" ]]; then
  shift
  exe="$(cargo test --quiet --manifest-path "$repo/Cargo.toml" --target $target --no-run --message-format=json \
    | python3 -c 'import json,sys; print([m["executable"] for m in map(json.loads, sys.stdin) if m.get("executable")][-1])')"
  cp "$exe" "$dest/steambro-tests.exe"
  exec "$dest/steambro-tests.exe" "$@"
fi

cargo build --quiet --manifest-path "$repo/Cargo.toml" --target $target
cp "$repo/target/$target/debug/steambro.exe" "$dest/"
exec "$dest/steambro.exe" "$@"
