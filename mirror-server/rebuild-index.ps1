# Scan public/mods/*.zip, read each mod's manifest.json, regenerate public/index.json.
# This is the prototype of the future upload/sync pipeline.
# Usage: powershell -ExecutionPolicy Bypass -File rebuild-index.ps1
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$pub  = Join-Path $root 'public'
$mods = Join-Path $pub 'mods'
if (-not (Test-Path $mods)) { New-Item -ItemType Directory -Path $mods | Out-Null }

$list = @()
Get-ChildItem $mods -Filter *.zip -File | Sort-Object Name | ForEach-Object {
    $zipPath = $_.FullName
    $name = $_.BaseName
    $ver = ''; $author = ''; $uid = ''; $desc = ''
    try {
        $zip = [IO.Compression.ZipFile]::OpenRead($zipPath)
        try {
            $entry = $zip.Entries |
                Where-Object { ($_.FullName -replace '\\', '/') -match '(^|/)manifest\.json$' } |
                Select-Object -First 1
            if ($entry) {
                $sr = New-Object IO.StreamReader($entry.Open())
                try { $raw = $sr.ReadToEnd() } finally { $sr.Close() }
                # Some authors ship JSONC manifests (block comments, rarely trailing commas).
                # Strip /* ... */ block comments (keep // inside strings like https://)
                # and trailing commas before } / ] so PS5 ConvertFrom-Json can parse them.
                $clean = [regex]::Replace($raw, '(?s)/\*.*?\*/', '')
                $clean = [regex]::Replace($clean, ',\s*([}\]])', '$1')
                try { $j = $clean | ConvertFrom-Json } catch { $j = $null }
                if ($j.Name)   { $name   = [string]$j.Name }
                $ver    = [string]$j.Version
                $author = [string]$j.Author
                $uid    = [string]$j.UniqueID
                $desc   = [string]$j.Description
            }
        } finally { $zip.Dispose() }
    } catch { Write-Host "WARN cannot read manifest in $($_.Name): $_" }

    $list += [ordered]@{
        name        = $name
        author      = $author
        version     = $ver
        unique_id   = $uid
        description = $desc
        file        = "mods/$($_.Name)"
        size        = $_.Length
    }
}

# Empty array serializes to null in PS5; -InputObject keeps non-empty arrays intact:
if ($list.Count -eq 0) {
    $json = '[]'
} else {
    $json = ConvertTo-Json -InputObject @($list) -Depth 4
}
[IO.File]::WriteAllText((Join-Path $pub 'index.json'), $json, (New-Object Text.UTF8Encoding($false)))
Write-Host "index.json rebuilt: $($list.Count) mod(s) -> $(Join-Path $pub 'index.json')"
