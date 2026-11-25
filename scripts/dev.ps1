# Builds steambro and runs the development copy, the one Steam's shortcuts start.
#   scripts\dev.ps1              start the tray app (blocks until it quits)
#   scripts\dev.ps1 list -v      run a command and print its output
#   scripts\dev.ps1 -Restart     end the running tray app, deploy, and start it again in the
#                                background; game launches that run keep their old copy
# Needs the GNU toolchain from rust-toolchain.toml and scoop's mingw (see "Build from source" in README.md).
param([switch] $Restart)
$ErrorActionPreference = 'Stop'

$repo = Split-Path $PSScriptRoot

# Steam starts the copy for its shortcuts, so it keeps one place while builds come and go, and a
# running copy doesn't lock the exe that cargo writes.
$dest = Join-Path $env:LOCALAPPDATA 'steambro-dev'
New-Item -ItemType Directory -Force $dest | Out-Null

cargo build --quiet --manifest-path "$repo\Cargo.toml"
if ($LASTEXITCODE) { exit $LASTEXITCODE }

if ($Restart) {
    # Only the tray app (no command, or `--background`): a game's `launch` and its `bridge-helper`
    # keep running.
    $tray = Get-CimInstance Win32_Process -Filter "Name='steambro.exe'" | Where-Object {
        $_.ExecutablePath -like "$dest\*" -and $_.CommandLine -notmatch '\s(launch|bridge-helper|list)\b'
    }
    foreach ($process in $tray) { Stop-Process -Id $process.ProcessId -Force }
    $tray | ForEach-Object { Wait-Process -Id $_.ProcessId -ErrorAction SilentlyContinue }
}

# With the GNU toolchain, WebView2's loader is a DLL next to the exe; MSVC builds link it in.
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME '.cargo' }
$loader = Get-ChildItem "$cargoHome\registry\src\*\webview2-com-sys-*\x64\WebView2Loader.dll" |
    Sort-Object { [version]($_.Directory.Parent.Name -replace '^webview2-com-sys-') } |
    Select-Object -Last 1
# A running game launch holds the loader too; it never changes, so it's copied only when it differs.
$present = Join-Path $dest 'WebView2Loader.dll'
if (-not (Test-Path $present) -or (Get-FileHash $present).Hash -ne (Get-FileHash $loader.FullName).Hash) {
    Copy-Item $loader.FullName $dest
}
$exe = "$dest\steambro.exe"
try {
    Copy-Item "$repo\target\debug\steambro.exe" $exe
} catch {
    # A running game launch holds the exe. Windows lets a running exe be renamed, so it moves aside
    # and keeps running from there.
    Remove-Item "$dest\steambro.old.exe" -ErrorAction SilentlyContinue
    Rename-Item $exe 'steambro.old.exe'
    Copy-Item "$repo\target\debug\steambro.exe" $exe
}

if ($Restart) {
    Start-Process $exe
    "steambro restarted from $exe"
    exit 0
}

# Through a pipe, PowerShell waits for the windowed exe and shows what it prints. WebView2 options
# reach it from the environment, e.g. WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222
& $exe @args | Out-Host
exit $LASTEXITCODE
