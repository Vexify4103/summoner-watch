# Builds a shareable installer: dist\com.vexify4103.summonerwatch.streamDeckPlugin
# Whoever gets the file double-clicks it and Stream Deck installs the plugin.
# Run from this folder:  powershell -ExecutionPolicy Bypass -File package.ps1
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot
$id = "com.vexify4103.summonerwatch.sdPlugin"

Write-Host "Building..."
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

if (-not (Test-Path "$id\imgs\key.png")) {
    powershell -ExecutionPolicy Bypass -File tools\make-icons.ps1
}
New-Item -ItemType Directory -Force "$id\bin" | Out-Null
Copy-Item target\release\summoner-watch.exe "$id\bin\summoner-watch.exe" -Force

# Elgato's official packer validates the manifest and writes the .streamDeckPlugin file.
New-Item -ItemType Directory -Force dist | Out-Null
npx --yes @elgato/cli@latest pack $id --output dist --force --no-file-list
if ($LASTEXITCODE -ne 0) { throw "streamdeck pack failed" }

Get-ChildItem dist\*.streamDeckPlugin | Select-Object Name, Length
