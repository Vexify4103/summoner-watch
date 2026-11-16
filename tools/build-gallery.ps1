# Captures the real plugin and renders the three Marketplace PNGs, offline.
param([switch]$SkipCapture)
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

if (-not $SkipCapture) {
    cargo build --release --bin summoner-watch --example fake_deck
    if ($LASTEXITCODE -ne 0) { throw "Plugin build failed" }
    cargo run --release --example fake_deck -- target/gallery-capture
    if ($LASTEXITCODE -ne 0) { throw "Plugin capture failed" }
    Copy-Item target/gallery-capture/*.svg marketing/keys/ -Force
    Copy-Item target/gallery-capture/capture.js marketing/capture.js -Force
}

$edge = "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe"
if (-not (Test-Path -LiteralPath $edge)) { throw "Microsoft Edge not found" }
$profile = Join-Path (Get-Location) "target/gallery-edge-profile"
foreach ($name in @("gallery-1-hero", "gallery-2-keys", "gallery-3-devices")) {
    $page = [Uri]::new((Join-Path (Get-Location) "marketing/$name.html")).AbsoluteUri
    $output = Join-Path (Get-Location) "marketing/$name.png"
    $arguments = @("--headless=new", "--disable-gpu", "--hide-scrollbars", "--force-device-scale-factor=1", "--window-size=1920,960", "--virtual-time-budget=2000", "--user-data-dir=`"$profile`"", "--screenshot=`"$output`"", $page)
    $process = Start-Process -FilePath $edge -ArgumentList $arguments -WindowStyle Hidden -PassThru
    if (-not $process.WaitForExit(30000)) { $process.Kill(); throw "Gallery render timed out" }
    if ($process.ExitCode -ne 0) { throw "Gallery render failed" }
    Write-Host "Rendered $output"
}
