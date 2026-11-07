# Builds the plugin's icons from assets\icon.png.
#   plugin icon (256/512) and default key image (72/144): resized artwork
#   action list + category icons: white single-color SVG (Elgato style guide)
# Run after changing the artwork: powershell -ExecutionPolicy Bypass -File tools\make-icons.ps1
Add-Type -AssemblyName System.Drawing
$root = Split-Path $PSScriptRoot -Parent
$src = Join-Path $root "assets\icon.png"
$out = Join-Path $root "com.vexify4103.summonerwatch.sdPlugin\imgs"
New-Item -ItemType Directory -Force $out | Out-Null

function Resize($name, $size) {
    $img = [System.Drawing.Image]::FromFile($src)
    $bmp = New-Object System.Drawing.Bitmap $size, $size
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = 'HighQualityBicubic'
    $g.SmoothingMode = 'HighQuality'
    $g.PixelOffsetMode = 'HighQuality'
    $g.CompositingQuality = 'HighQuality'
    $g.DrawImage($img, 0, 0, $size, $size)
    $bmp.Save((Join-Path $out "$name.png"), [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose(); $img.Dispose()
}

Resize "plugin" 256
Resize "plugin@2x" 512
Resize "key" 72
Resize "key@2x" 144

# Stopwatch with an eye: white strokes on transparent background.
$svg = @'
<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="#FFFFFF" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
  <circle cx="10" cy="11.5" r="7"/>
  <path d="M8 2.5h4M10 2.5v2"/>
  <path d="M5.5 11.5c1.3-2 2.8-3 4.5-3s3.2 1 4.5 3c-1.3 2-2.8 3-4.5 3s-3.2-1-4.5-3z"/>
  <circle cx="10" cy="11.5" r="1.2" fill="#FFFFFF" stroke="none"/>
</svg>
'@
foreach ($name in "action", "category") {
    [IO.File]::WriteAllText((Join-Path $out "$name.svg"), $svg)
    foreach ($old in "$name.png", "$name@2x.png") {
        $p = Join-Path $out $old
        if (Test-Path $p) { Remove-Item $p }
    }
}
Write-Output "icons written to $out"
