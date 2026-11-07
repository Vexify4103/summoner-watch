# Builds the plugin and installs it into Stream Deck.
# Run from this folder:  powershell -ExecutionPolicy Bypass -File install.ps1
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

$id = "com.vexify4103.summonerwatch.sdPlugin"
$dest = Join-Path $env:APPDATA "Elgato\StreamDeck\Plugins\$id"
$streamDeck = "C:\Program Files\Elgato\StreamDeck\StreamDeck.exe"

Write-Host "Building..."
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

if (-not (Test-Path "$id\imgs\key.png")) {
    powershell -ExecutionPolicy Bypass -File tools\make-icons.ps1
}

New-Item -ItemType Directory -Force "$id\bin" | Out-Null
Copy-Item target\release\summoner-watch.exe "$id\bin\summoner-watch.exe" -Force

Write-Host "Closing Stream Deck..."
Get-Process StreamDeck -ErrorAction SilentlyContinue | Stop-Process
Get-Process summoner-watch -ErrorAction SilentlyContinue | Stop-Process
Start-Sleep -Seconds 2

# Remove the pre-release build that used an older plugin ID.
$legacy = Join-Path $env:APPDATA "Elgato\StreamDeck\Plugins\com.rumi.lolsumtimer.sdPlugin"
if (Test-Path $legacy) { Remove-Item $legacy -Recurse -Force }

Write-Host "Copying plugin to $dest"
if (Test-Path $dest) { Remove-Item $dest -Recurse -Force }
Copy-Item $id $dest -Recurse

Write-Host "Starting Stream Deck..."
Start-Process $streamDeck
Write-Host "Done. Look for the 'Summoner Watch' category in the Stream Deck action list."
