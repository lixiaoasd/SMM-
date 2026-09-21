# Backfill category_id for every already-mirrored mod.
# Parses mod id from index.json file names (mods/n{id}-{fid}.zip),
# calls mods/{id}.json (cached raw JSON in meta-cache/), adds category_id,
# and rewrites index.json (order preserved by _downloads; schema otherwise unchanged).
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File backfill-category.ps1 [-Workers 6]
param(
    [int]$Workers = 6
)
$ErrorActionPreference = 'Stop'

$root      = Split-Path -Parent $MyInvocation.MyCommand.Path
$indexPath = Join-Path $root 'public\index.json'
$cacheDir  = Join-Path $root 'meta-cache'
if (-not (Test-Path $cacheDir)) { New-Item -ItemType Directory -Path $cacheDir | Out-Null }

$keyFile = Join-Path $root 'nexus-api.key'
$apiKey  = (Get-Content $keyFile -Raw).Trim()
$apiBase = 'https://api.nexusmods.com/v1/games/stardewvalley'

Write-Host "index: $indexPath"
$raw  = [IO.File]::ReadAllText($indexPath)
Write-Host "raw length: $($raw.Length)"
$parsed = $raw | ConvertFrom-Json
Write-Host "parsed type: $($parsed.GetType().FullName)"
$list  = @($parsed)
Write-Host "list count: $($list.Count)"
$tasks = New-Object System.Collections.ArrayList
foreach ($e in $list) {
    $rm = [regex]::Match([string]$e.file, 'n(\d+)-\d+\.zip$')
    if ($rm.Success) {
        [void]$tasks.Add([pscustomobject]@{ Id = [int]$rm.Groups[1].Value; Entry = $e })
    }
}
Write-Host "entries: $($list.Count); nexus mods to resolve: $($tasks.Count)"

# ---------- worker ----------
$worker = {
    param($id, $apiKey, $apiBase, $cacheDir)
    $ProgressPreference = 'SilentlyContinue'
    $cacheFile = Join-Path $cacheDir "$id.json"
    $m = $null
    if (Test-Path $cacheFile) {
        try { $m = ConvertFrom-Json ([IO.File]::ReadAllText($cacheFile)) } catch { $m = $null }
    }
    if (-not $m) {
        $h = @{ apikey = $apiKey }
        for ($attempt = 0; $attempt -lt 40; $attempt++) {
            try {
                $resp = Invoke-WebRequest "$apiBase/mods/$id.json" -Headers $h -UseBasicParsing -TimeoutSec 45
                [IO.File]::WriteAllText($cacheFile, $resp.Content, (New-Object Text.UTF8Encoding($false)))
                $m = $resp.Content | ConvertFrom-Json
                break
            } catch {
                $code = 0; $retryAfter = 0
                if ($_.Exception.Response) {
                    $code = [int]$_.Exception.Response.StatusCode
                    $ra = $_.Exception.Response.Headers['Retry-After']
                    if ($ra) { [int]::TryParse($ra, [ref]$retryAfter) | Out-Null }
                }
                if ($code -eq 429 -or $code -eq 503) {
                    if ($retryAfter -le 0) { $retryAfter = 30 }
                    Start-Sleep -Seconds ([Math]::Min($retryAfter, 1800) + 1); continue
                }
                if ($code -eq 404) { return [pscustomobject]@{ Ok = $false; Category = 0; Downloads = 0 } }
                if ($code -ge 500) { Start-Sleep -Seconds 5; continue }
                throw
            }
        }
        if (-not $m) { throw "rate-limit retries exhausted: $id" }
    }
    [pscustomobject]@{
        Ok        = $true
        Category  = [int]$m.category_id
        Downloads = [int64]$m.mod_downloads
    }
}

$iss = [System.Management.Automation.Runspaces.InitialSessionState]::CreateDefault()
$iss.ApartmentState = 'MTA'
$pool = [RunspaceFactory]::CreateRunspacePool(1, $Workers, $iss, $Host)
$pool.Open()
$jobs = @()
foreach ($t in $tasks) {
    $ps = [PowerShell]::Create()
    $ps.RunspacePool = $pool
    [void]$ps.AddScript($worker).AddArgument($t.Id).AddArgument($apiKey).AddArgument($apiBase).AddArgument($cacheDir)
    $jobs += [pscustomobject]@{ Task = $t; PS = $ps; Handle = $ps.BeginInvoke() }
}

$done = 0; $fail = 0
$infoOf = @{}
foreach ($j in $jobs) {
    $r = $j.PS.EndInvoke($j.Handle)
    $j.PS.Dispose()
    $res = $r | Select-Object -First 1
    $done++
    if ($res -and $res.Ok) {
        $infoOf[$j.Task.Id] = $res
    } else {
        $fail++
        Write-Host "  ! failed id=$($j.Task.Id)"
    }
    if ($done % 50 -eq 0) { Write-Host "progress: $done/$($jobs.Count) fail=$fail" }
}
$pool.Close(); $pool.Dispose()
Write-Host "resolved: $done, failed: $fail"

# ---------- merge into index.json ----------
$out = New-Object System.Collections.ArrayList
foreach ($t in $tasks) {
    $e = $t.Entry
    $cat = 0; $downloads = 0
    if ($infoOf.ContainsKey($t.Id)) {
        $cat       = [int]$infoOf[$t.Id].Category
        $downloads = [int64]$infoOf[$t.Id].Downloads
    }
    [void]$out.Add([pscustomobject][ordered]@{
        name        = [string]$e.name
        author      = [string]$e.author
        version     = [string]$e.version
        unique_id   = [string]$e.unique_id
        description = [string]$e.description
        category_id = [int]$cat
        file        = [string]$e.file
        size        = [int64]$e.size
        _downloads  = [int64]$downloads
    })
}
# Entries without n-prefixed files (none today, but keep them)
foreach ($e in $list) {
    if ($e.file -notmatch 'n(\d+)-\d+\.zip$') {
        [void]$out.Add($e)
    }
}
$sorted = $out | Sort-Object @{Expression={ [int64]$_._downloads }; Descending=$true}
$final = @($sorted | ForEach-Object {
    [ordered]@{
        name        = $_.name
        author      = $_.author
        version     = $_.version
        unique_id   = $_.unique_id
        description = $_.description
        category_id = [int]$_.category_id
        file        = $_.file
        size        = [int64]$_.size
        _downloads  = [int64]$_._downloads
    }
})
$json = ConvertTo-Json -InputObject $final -Depth 4
[IO.File]::WriteAllText($indexPath, $json, (New-Object Text.UTF8Encoding($false)))

$distinct = @($catOf.Values | Sort-Object -Unique)
Write-Host ("DONE: indexed={0} category_ids={1} ({2})" -f $final.Count, $distinct.Count, ($distinct -join ','))
