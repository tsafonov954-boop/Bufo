# Builds dist\bufo.exe: ONE file with ffmpeg embedded (no install, no download).
# Needs: Rust, ffmpeg.exe (path auto-detected or pass -Ffmpeg), and xz on PATH (e.g. Git for Windows).
param([string]$Ffmpeg = "")
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
if (-not $Ffmpeg) {
    $Ffmpeg = (Get-Command ffmpeg -ErrorAction SilentlyContinue).Source
    if (-not $Ffmpeg) {
        $Ffmpeg = Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Recurse -Filter ffmpeg.exe -ErrorAction SilentlyContinue |
            Select-Object -First 1 -ExpandProperty FullName
    }
}
if (-not $Ffmpeg -or -not (Test-Path $Ffmpeg)) { throw "ffmpeg.exe not found; pass -Ffmpeg <path>" }
$xz = (Get-Command xz -ErrorAction SilentlyContinue).Source
if (-not $xz) { foreach ($p in "C:\Program Files\Git\usr\bin\xz.exe", "C:\msys64\ucrt64\bin\xz.exe") { if (Test-Path $p) { $xz = $p; break } } }
if (-not $xz) { throw "xz not found (install Git for Windows or MSYS2)" }
New-Item -ItemType Directory -Force dist | Out-Null
$blob = Join-Path $root "dist\ffmpeg.exe.xz"
if (-not (Test-Path $blob) -or (Get-Item $blob).LastWriteTime -lt (Get-Item $Ffmpeg).LastWriteTime) {
    Write-Host "compressing $Ffmpeg (about 1-2 minutes) ..."
    cmd /c "`"$xz`" -9e -T0 -c `"$Ffmpeg`" > `"$blob`""
}
$env:BUFO_FFMPEG_XZ = $blob
cargo build --release --features bundle-ffmpeg
Copy-Item target\release\bufo.exe dist\bufo.exe -Force
"{0}: {1:N1} MB" -f "dist\bufo.exe", ((Get-Item dist\bufo.exe).Length / 1MB)
