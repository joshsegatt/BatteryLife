# Draws the app icon (signal-yellow square, black battery pictogram) into a multi-size .ico.
# Usage: pwsh app/tools/make-icon.ps1   (writes app/BatteryLife.App/Assets/app.ico)
Add-Type -AssemblyName System.Drawing
$out = Join-Path $PSScriptRoot '..\BatteryLife.App\Assets\app.ico'
New-Item -ItemType Directory -Force (Split-Path $out) | Out-Null

$yellow = [System.Drawing.Color]::FromArgb(255, 0xFF, 0xD2, 0x00)
$ink = [System.Drawing.Color]::FromArgb(255, 0x11, 0x11, 0x11)
$frames = foreach ($s in 16, 24, 32, 48, 64, 256) {
    $bmp = New-Object System.Drawing.Bitmap $s, $s
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.Clear([System.Drawing.Color]::Transparent)
    $r = [Math]::Max(2, $s * 0.18)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddArc(0, 0, 2 * $r, 2 * $r, 180, 90); $path.AddArc($s - 2 * $r, 0, 2 * $r, 2 * $r, 270, 90)
    $path.AddArc($s - 2 * $r, $s - 2 * $r, 2 * $r, 2 * $r, 0, 90); $path.AddArc(0, $s - 2 * $r, 2 * $r, 2 * $r, 90, 90)
    $path.CloseFigure()
    $g.FillPath((New-Object System.Drawing.SolidBrush $yellow), $path)
    # Battery body (outline), nub and a three-quarter charge bar, all in ink.
    $pw = [Math]::Max(1.5, $s * 0.075)
    $pen = New-Object System.Drawing.Pen $ink, $pw
    $bx = $s * 0.18; $by = $s * 0.32; $bw = $s * 0.56; $bh = $s * 0.36
    $g.DrawRectangle($pen, [float]$bx, [float]$by, [float]$bw, [float]$bh)
    $g.FillRectangle((New-Object System.Drawing.SolidBrush $ink), [float]($bx + $bw + $pw / 2), [float]($by + $bh * 0.3), [float]($s * 0.07), [float]($bh * 0.4))
    $inset = $pw * 1.6
    $g.FillRectangle((New-Object System.Drawing.SolidBrush $ink), [float]($bx + $inset), [float]($by + $inset), [float](($bw - 2 * $inset) * 0.72), [float]($bh - 2 * $inset))
    $g.Dispose()
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    [pscustomobject]@{ Size = $s; Bytes = $ms.ToArray() }
}

$fs = [System.IO.File]::Create($out)
$w = New-Object System.IO.BinaryWriter $fs
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$frames.Count)
$offset = 6 + 16 * $frames.Count
foreach ($f in $frames) {
    $dim = if ($f.Size -ge 256) { 0 } else { $f.Size }
    $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32); $w.Write([uint32]$f.Bytes.Length); $w.Write([uint32]$offset)
    $offset += $f.Bytes.Length
}
foreach ($f in $frames) { $w.Write($f.Bytes) }
$w.Close()
"wrote $out"
