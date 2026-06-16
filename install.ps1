# knock installer (Windows) — downloads the latest release binary.
#
#   irm https://raw.githubusercontent.com/hihenen/knock/master/install.ps1 | iex
#
# Installs knock.exe to %LOCALAPPDATA%\knock and adds it to the user PATH.

$ErrorActionPreference = "Stop"

$repo = "hihenen/knock"
$asset = "knock-windows-x64.exe"
$destDir = Join-Path $env:LOCALAPPDATA "knock"
$dest = Join-Path $destDir "knock.exe"

# /releases/latest/download redirects to the newest asset (no GitHub API call).
$url = "https://github.com/$repo/releases/latest/download/$asset"

New-Item -ItemType Directory -Force -Path $destDir | Out-Null

Write-Host "knock: downloading latest release..."
Invoke-WebRequest -Uri $url -OutFile $dest

# Add to user PATH if missing.
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$destDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$destDir", "User")
    Write-Host "knock: added $destDir to your user PATH (restart the terminal to use 'knock')."
}

Write-Host "knock: installed to $dest"
& $dest --version

Write-Host ""
Write-Host "Usage:"
Write-Host "  knock annotate <file.md> --gate --json   # approval / annotation gate"
Write-Host "  knock ask <questions.json>               # multiple-choice question"
