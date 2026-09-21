# 生成应用图标：绿色渐变圆角方块 + 白色五角星
# 产物：assets/app.ico（多尺寸）、assets/icon_256.rgba（窗口图标原始字节）、assets/icon_256.png
Add-Type -AssemblyName System.Drawing

$outDir = "C:\Users\123\Desktop\FireSVM-Glass\assets"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

function New-IconBitmap([int]$size) {
    $bmp = [System.Drawing.Bitmap]::new($size, $size)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)

    $radius = [int]($size * 0.22)
    $rect = [System.Drawing.Rectangle]::new(0, 0, ($size - 1), ($size - 1))
    $path = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $d = $radius * 2
    $path.AddArc($rect.X, $rect.Y, $d, $d, 180, 90)
    $path.AddArc(($rect.Right - $d), $rect.Y, $d, $d, 270, 90)
    $path.AddArc(($rect.Right - $d), ($rect.Bottom - $d), $d, $d, 0, 90)
    $path.AddArc($rect.X, ($rect.Bottom - $d), $d, $d, 90, 90)
    $path.CloseFigure()

    $c1 = [System.Drawing.Color]::FromArgb(255, 92, 184, 92)
    $c2 = [System.Drawing.Color]::FromArgb(255, 27, 94, 32)
    $brush = [System.Drawing.Drawing2D.LinearGradientBrush]::new($rect, $c1, $c2, 135.0)
    $g.FillPath($brush, $path)

    $cx = $size / 2.0
    $cy = $size / 2.0 + $size * 0.02
    $outer = $size * 0.32
    $inner = $outer * 0.42
    $pts = [System.Drawing.PointF[]]::new(10)
    for ($i = 0; $i -lt 10; $i++) {
        if ($i % 2 -eq 0) { $r = $outer } else { $r = $inner }
        $ang = -[Math]::PI / 2 + $i * [Math]::PI / 5
        $px = [float]($cx + $r * [Math]::Cos($ang))
        $py = [float]($cy + $r * [Math]::Sin($ang))
        $pts[$i] = [System.Drawing.PointF]::new($px, $py)
    }
    $starBrush = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::White)
    $g.FillPolygon($starBrush, $pts)

    $g.Dispose()
    return $bmp
}

$bmp256 = New-IconBitmap 256
$bmp256.Save((Join-Path $outDir "icon_256.png"), [System.Drawing.Imaging.ImageFormat]::Png)

# 导出 RGBA 原始字节（窗口图标用；逐像素读取，一次性脚本无需追求性能）
$rgba = [byte[]]::new(256 * 256 * 4)
for ($y = 0; $y -lt 256; $y++) {
    for ($x = 0; $x -lt 256; $x++) {
        $c = $bmp256.GetPixel($x, $y)
        $idx = ($y * 256 + $x) * 4
        $rgba[$idx]     = $c.R
        $rgba[$idx + 1] = $c.G
        $rgba[$idx + 2] = $c.B
        $rgba[$idx + 3] = $c.A
    }
}
[System.IO.File]::WriteAllBytes((Join-Path $outDir "icon_256.rgba"), $rgba)

# 多尺寸 ICO（PNG 压缩条目，Vista+ 支持）
$sizes = @(256, 128, 64, 48, 32, 16)
$pngBlobs = @()
foreach ($s in $sizes) {
    if ($s -eq 256) { $b = $bmp256 } else { $b = New-IconBitmap $s }
    $ms = [System.IO.MemoryStream]::new()
    $b.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $pngBlobs += , $ms.ToArray()
    if ($s -ne 256) { $b.Dispose() }
    $ms.Dispose()
}

$fs = [System.IO.File]::Create((Join-Path $outDir "app.ico"))
$bw = [System.IO.BinaryWriter]::new($fs)
$bw.Write([uint16]0)
$bw.Write([uint16]1)
$bw.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $s = $sizes[$i]
    $blob = $pngBlobs[$i]
    if ($s -ge 256) { $dim = [byte]0 } else { $dim = [byte]$s }
    $bw.Write($dim)
    $bw.Write($dim)
    $bw.Write([byte]0)
    $bw.Write([byte]0)
    $bw.Write([uint16]1)
    $bw.Write([uint16]32)
    $bw.Write([uint32]$blob.Length)
    $bw.Write([uint32]$offset)
    $offset += $blob.Length
}
foreach ($blob in $pngBlobs) { $bw.Write($blob) }
$bw.Close(); $fs.Close()
$bmp256.Dispose()

"图标已生成："
Get-ChildItem $outDir | ForEach-Object { "  $($_.Name)  $([math]::Round($_.Length/1KB,1)) KB" }
