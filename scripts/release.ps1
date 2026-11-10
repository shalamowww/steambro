# Builds a release of steambro: a portable zip and an installer.
#   scripts\release.ps1          writes target\release\steambro-v<version>-windows-x64.zip
#                                and, with NSIS, steambro-v<version>-windows-x64-setup.exe
# Both hold steambro.exe, WebView2Loader.dll, and LICENSE.txt (GPL-3.0).
# <version> is Cargo.toml's version with a fourth number of 0, as the Microsoft Store writes it:
# the release tag (v<version>), the files, the installer and the Store package carry one number.
# Needs the GNU toolchain from rust-toolchain.toml and scoop's mingw; the installer needs NSIS
# (scoop install nsis, from the extras bucket), or MAKENSIS pointing at makensis.exe.
# Run it in a clean export of a commit (see "The scripts" in README.md): the exe holds the ui folder as it
# is, so files that git doesn't track would go into it.
$ErrorActionPreference = 'Stop'

$repo = Split-Path $PSScriptRoot
$version = (Select-String -Path "$repo\Cargo.toml" -Pattern '^version = "(.*)"' | Select-Object -First 1).Matches.Groups[1].Value + '.0'
$out = "$repo\target\release"
$name = "steambro-v$version-windows-x64"

cargo build --quiet --release --manifest-path "$repo\Cargo.toml"
if ($LASTEXITCODE) { exit $LASTEXITCODE }

# With the GNU toolchain, WebView2's loader is a DLL that must sit next to the exe.
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME '.cargo' }
$loader = Get-ChildItem "$cargoHome\registry\src\*\webview2-com-sys-*\x64\WebView2Loader.dll" |
    Sort-Object { [version]($_.Directory.Parent.Name -replace '^webview2-com-sys-') } |
    Select-Object -Last 1
$stage = Join-Path ([IO.Path]::GetTempPath()) "steambro-release-$([guid]::NewGuid())"
$files = "$stage\steambro"
try {
    New-Item -ItemType Directory -Force $files | Out-Null
    Copy-Item "$repo\target\release\steambro.exe", $loader.FullName $files
    Copy-Item "$repo\LICENSE" "$files\LICENSE.txt"
    Remove-Item "$out\$name.zip" -ErrorAction SilentlyContinue
    # Windows' tar, because Compress-Archive in Windows PowerShell writes backslashes into the zip.
    tar.exe -a -c -f "$out\$name.zip" -C $stage steambro
    if ($LASTEXITCODE) { exit $LASTEXITCODE }
    "$out\$name.zip"

    $makensis = if ($env:MAKENSIS) { $env:MAKENSIS } else { (Get-Command makensis -ErrorAction SilentlyContinue).Source }
    if (-not $makensis) {
        Write-Warning 'no makensis, so no installer'
        exit 0
    }
    & $makensis -V2 "-DVERSION=$version" "-DEXE=$files\steambro.exe" "-DLOADER=$files\WebView2Loader.dll" `
        "-DLICENSE=$files\LICENSE.txt" `
        "-DICON=$repo\icons\icon.ico" "-DOUT=$out\$name-setup.exe" "$repo\scripts\installer.nsi"
    if ($LASTEXITCODE) { exit $LASTEXITCODE }
    "$out\$name-setup.exe"
} finally {
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
}
