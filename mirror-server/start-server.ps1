# Stardew Mod Manager local mirror server (zero-dependency: built-in .NET HttpListener)
# Usage: powershell -ExecutionPolicy Bypass -File start-server.ps1 [port]
# Default: http://localhost:8770/  ->  serves ./public
param([int]$Port = 8770)

$ErrorActionPreference = 'Stop'
$root   = Split-Path -Parent $MyInvocation.MyCommand.Path
$public = Join-Path $root 'public'
if (-not (Test-Path $public)) { New-Item -ItemType Directory -Path $public | Out-Null }

$prefix = "http://localhost:$Port/"
$listener = New-Object System.Net.HttpListener
$listener.Prefixes.Add($prefix)
$listener.Start()

Write-Host "================================================"
Write-Host " Stardew Mod Manager mirror server"
Write-Host " root : $public"
Write-Host " url  : $prefix"
Write-Host " index: $prefixindex.json"
Write-Host " stop : Ctrl+C"
Write-Host "================================================"

$types = @{
    '.zip'   = 'application/zip'
    '.json'  = 'application/json; charset=utf-8'
    '.txt'   = 'text/plain; charset=utf-8'
    '.png'   = 'image/png'
    '.jpg'   = 'image/jpeg'
    '.webp'  = 'image/webp'
    '.md'    = 'text/plain; charset=utf-8'
}

try {
    while ($listener.IsListening) {
        $ctx = $listener.GetContext()
        try {
            $rel = [Uri]::UnescapeDataString($ctx.Request.Url.AbsolutePath.TrimStart('/'))
            if ([string]::IsNullOrEmpty($rel)) { $rel = 'index.json' }
            $rel = $rel -replace '/', '\'
            $file = Join-Path $public $rel
            $full    = [System.IO.Path]::GetFullPath($file)
            $pubFull = [System.IO.Path]::GetFullPath($public) + [System.IO.Path]::DirectorySeparatorChar

            if (-not $full.StartsWith($pubFull) -or -not (Test-Path $full -PathType Leaf)) {
                $ctx.Response.StatusCode = 404
                $b = [Text.Encoding]::UTF8.GetBytes("not found: $rel")
                $ctx.Response.OutputStream.Write($b, 0, $b.Length)
                Write-Host ("404 {0}" -f $rel)
            }
            else {
                $ext = [IO.Path]::GetExtension($full).ToLower()
                if ($types.ContainsKey($ext)) { $ctx.Response.ContentType = $types[$ext] }
                else { $ctx.Response.ContentType = 'application/octet-stream' }
                $len = (Get-Item $full).Length
                $ctx.Response.ContentLength64 = $len
                $ctx.Response.AddHeader('Cache-Control', 'no-cache')
                $fs = [IO.File]::OpenRead($full)
                try { $fs.CopyTo($ctx.Response.OutputStream) } finally { $fs.Close() }
                Write-Host ("200 {0,12:N0}  {1}" -f $len, $rel)
            }
        }
        catch { Write-Host "ERR $_" }
        finally { $ctx.Response.Close() }
    }
}
finally { $listener.Stop() }
