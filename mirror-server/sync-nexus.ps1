# Sync ~1000 popular SDV mods from Nexus Mods into the local mirror.
# Requires: mirror-server/nexus-api.key containing a premium API key.
# Flow per mod: mods/{id}.json -> files.json -> premium download_link -> download zip
# -> read manifest.json inside zip -> append to public/index.json (rewritten periodically).
# Resumable: existing zips are reused; re-running continues where it stopped.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File sync-nexus.ps1 [-Period 1w] [-Workers 6] [-Limit 0]
param(
    [string]$Period = '1w',          # updated.json period: 1d | 1w | 1m
    [int]$Workers = 6,
    [int]$Limit = 0,                 # 0 = all
    [int]$MaxFileKb = 0              # 0 = no size cap; e.g. 204800 skips files > 200 MB
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem

$root  = Split-Path -Parent $MyInvocation.MyCommand.Path
$modsDir = Join-Path $root 'public\mods'
$indexPath = Join-Path $root 'public\index.json'
if (-not (Test-Path $modsDir)) { New-Item -ItemType Directory -Path $modsDir | Out-Null }

$keyFile = Join-Path $root 'nexus-api.key'
if (-not (Test-Path $keyFile)) { throw "missing nexus-api.key" }
$apiKey = (Get-Content $keyFile -Raw).Trim()
$apiBase = 'https://api.nexusmods.com/v1/games/stardewvalley'

# category_id -> @{ en; zh } table (see apply-categories.ps1)
$catMap = @{}
$catPath = Join-Path $root 'categories.json'
if (Test-Path $catPath) {
    $catRaw = [IO.File]::ReadAllText($catPath) | ConvertFrom-Json
    foreach ($prop in $catRaw.PSObject.Properties) {
        $catMap[[int]$prop.Name] = $prop.Value
    }
}

# ---------- shared state ----------
$entriesList = [System.Collections.ArrayList]::Synchronized((New-Object System.Collections.ArrayList))
$gate = [object]::new()
$sync = [hashtable]::Synchronized(@{
    Entries = $entriesList
    Done    = 0
    Failed  = 0
    Skipped = 0
    Bytes   = [int64]0
})
$failedLog = Join-Path $root 'sync-failed.log'
if (Test-Path $failedLog) { Remove-Item $failedLog -Force }

function Write-Index {
    param($List)
    if ($List.Count -eq 0) { $json = '[]' }
    else { $json = ConvertTo-Json -InputObject @($List) -Depth 4 }
    [IO.File]::WriteAllText($indexPath, $json, (New-Object Text.UTF8Encoding($false)))
}

# Read manifest.json out of a mod zip; returns $null when absent/invalid.
function Read-ZipManifest($zipPath) {
    try {
        $zip = [IO.Compression.ZipFile]::OpenRead($zipPath)
        try {
            $entry = $zip.Entries |
                Where-Object { ($_.FullName -replace '\\', '/') -match '(^|/)manifest\.json$' } |
                Select-Object -First 1
            if (-not $entry) { return $null }
            $sr = New-Object IO.StreamReader($entry.Open())
            try { $raw = $sr.ReadToEnd() } finally { $sr.Close() }
            $clean = [regex]::Replace($raw, '(?s)/\*.*?\*/', '')
            $clean = [regex]::Replace($clean, ',\s*([}\]])', '$1')
            try { return ($clean | ConvertFrom-Json) } catch { return $null }
        } finally { $zip.Dispose() }
    } catch { return $null }
}

# ---------- worker (self-contained; PS5 runspaces) ----------
$worker = {
    param($modId, $apiKey, $apiBase, $modsDir, $sync, $failedLog, $MaxFileKb, $gate, $CatMap)

    $ProgressPreference = 'SilentlyContinue'

    # API GET with rate-limit handling (429/503 -> honor Retry-After).
    function Invoke-Nexus($url, $headers) {
        for ($attempt = 0; $attempt -lt 40; $attempt++) {
            try {
                return Invoke-RestMethod -Uri $url -Headers $headers -TimeoutSec 45
            } catch {
                $code = 0
                $retryAfter = 0
                if ($_.Exception.Response) {
                    $code = [int]$_.Exception.Response.StatusCode
                    $ra = $_.Exception.Response.Headers['Retry-After']
                    if ($ra) { [int]::TryParse($ra, [ref]$retryAfter) | Out-Null }
                }
                if ($code -eq 429 -or $code -eq 503) {
                    if ($retryAfter -le 0) { $retryAfter = 30 }
                    $retryAfter = [Math]::Min($retryAfter, 1800)
                    Start-Sleep -Seconds ($retryAfter + 1)
                    continue
                }
                if ($code -ge 500) { Start-Sleep -Seconds 5; continue }
                throw
            }
        }
        throw "rate-limit retries exhausted"
    }

    # Read manifest.json out of a mod zip (self-contained; pool runspaces have
    # no access to main-scope functions).
    function Read-ZipManifest($zipPath) {
        try {
            $zip = [IO.Compression.ZipFile]::OpenRead($zipPath)
            try {
                $entry = $zip.Entries |
                    Where-Object { ($_.FullName -replace '\\', '/') -match '(^|/)manifest\.json$' } |
                    Select-Object -First 1
                if (-not $entry) { return $null }
                $sr = New-Object IO.StreamReader($entry.Open())
                try { $raw = $sr.ReadToEnd() } finally { $sr.Close() }
                $clean = [regex]::Replace($raw, '(?s)/\*.*?\*/', '')
                $clean = [regex]::Replace($clean, ',\s*([}\]])', '$1')
                try { return ($clean | ConvertFrom-Json) } catch { return $null }
            } finally { $zip.Dispose() }
        } catch { return $null }
    }
    function Bump-Skip {
        [System.Threading.Monitor]::Enter($gate)
        try { $sync.Skipped++ } finally { [System.Threading.Monitor]::Exit($gate) }
    }

    $h = @{ apikey = $apiKey }
    try {
        # 1) metadata
        $m = Invoke-Nexus "$apiBase/mods/$modId.json" $h
        if ($null -eq $m) { Bump-Skip; return }
        $available = [bool]$m.available
        $status = [string]$m.status
        if (-not $available -or $status -eq 'not_published' -or $status -eq 'removed') {
            Bump-Skip; return
        }

        # 2) pick primary file
        $fo = Invoke-Nexus "$apiBase/mods/$modId/files.json" $h
        $files = @($fo.files)
        if ($files.Count -eq 0) { throw "no files" }
        $primary = $files | Where-Object { $_.is_primary } | Select-Object -First 1
        if (-not $primary) {
            $primary = $files | Where-Object { $_.category_name -eq 'MAIN' } |
                Sort-Object file_id -Descending | Select-Object -First 1
        }
        if (-not $primary) {
            $primary = $files | Where-Object { $_.category_name -ne 'OLD_VERSION' } |
                Sort-Object file_id -Descending | Select-Object -First 1
        }
        $fid = [int64]$primary.file_id
        $sizeKb = [int64]$primary.size_kb
        if ($MaxFileKb -gt 0 -and $sizeKb -gt $MaxFileKb) {
            Bump-Skip; return
        }

        $fname = "n$modId-$fid.zip"
        $fpath = Join-Path $modsDir $fname
        $needDownload = $true
        if ((Test-Path $fpath) -and (Get-Item $fpath).Length -gt 0) { $needDownload = $false }

        if ($needDownload) {
            # 3) premium CDN link
            $links = Invoke-Nexus "$apiBase/mods/$modId/files/$fid/download_link.json?key=premium" $h
            if (-not $links -or $links.Count -eq 0 -or -not $links[0].URI) { throw "no cdn link" }
            $tmp = "$fpath.part"
            Invoke-WebRequest -Uri $links[0].URI -OutFile $tmp -UseBasicParsing -TimeoutSec 900
            if (-not (Test-Path $tmp) -or (Get-Item $tmp).Length -eq 0) { throw "empty download" }
            Move-Item $tmp $fpath -Force
        }

        # 4) verify zip + read manifest for accurate identity
        $info = Get-Item $fpath
        $manifest = Read-ZipManifest $fpath
        $cid = [int]$m.category_id
        $catZh = ''; $catEn = ''
        if ($CatMap -and $CatMap.ContainsKey($cid)) {
            $catZh = [string]$CatMap[$cid].zh
            $catEn = [string]$CatMap[$cid].en
        }
        $entry = [ordered]@{
            mod_id      = [int]$modId
            name        = if ($manifest -and $manifest.Name) { [string]$manifest.Name } else { [string]$m.name }
            author      = if ($manifest -and $manifest.Author) { [string]$manifest.Author } else { [string]$m.user.name }
            version     = if ($manifest -and $manifest.Version) { [string]$manifest.Version } else { [string]$m.version }
            unique_id   = if ($manifest -and $manifest.UniqueID) { [string]$manifest.UniqueID } else { "" }
            description = if ($manifest -and $manifest.Description) { [string]$manifest.Description } else { [string]$m.summary }
            category_id = $cid
            category    = $catZh
            category_en = $catEn
            file        = "mods/$fname"
            size        = [int64]$info.Length
            _downloads  = [int64]$m.mod_downloads
        }

        [System.Threading.Monitor]::Enter($gate)
        try {
            [void]$sync.Entries.Add($entry)
            $sync.Done++
            $sync.Bytes += [int64]$info.Length
        } finally { [System.Threading.Monitor]::Exit($gate) }
    } catch {
        [System.Threading.Monitor]::Enter($gate)
        try {
            $sync.Failed++
            Add-Content -Path $failedLog -Value "$modId`t$($_.Exception.Message)"
        } finally { [System.Threading.Monitor]::Exit($gate) }
    }
}

# ---------- get target mod ids ----------
function Invoke-NexusMain($url, $headers) {
    for ($attempt = 0; $attempt -lt 40; $attempt++) {
        try {
            return Invoke-RestMethod -Uri $url -Headers $headers -TimeoutSec 45
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
            if ($code -ge 500) { Start-Sleep -Seconds 5; continue }
            throw
        }
    }
    throw "rate-limit retries exhausted"
}

Write-Host "fetching updated mod ids (period=$Period) ..."
$upd = Invoke-NexusMain "$apiBase/mods/updated.json?period=$Period" @{ apikey = $apiKey }
$ids = @($upd | ForEach-Object { [int]$_.mod_id })
if ($Limit -gt 0) { $ids = $ids | Select-Object -First $Limit }
Write-Host "target mods: $($ids.Count)"

# Reuse entries already indexed for zips present on disk (resume across re-runs).
$existing = @{}
if (Test-Path $indexPath) {
    try {
        $old = [IO.File]::ReadAllText($indexPath) | ConvertFrom-Json
        foreach ($e in @($old)) {
            $mt = [regex]::Match([string]$e.file, 'n(\d+)-\d+\.zip$')
            if ($mt.Success) {
                $mid = [int]$mt.Groups[1].Value
                $zp = Join-Path $root ("public\" + ([string]$e.file -replace '/','\'))
                if (Test-Path $zp) { $existing[$mid] = $e }
            }
        }
    } catch {}
}
foreach ($kv in $existing.GetEnumerator()) {
    $e = $kv.Value
    $mid = [int]$kv.Key
    $entry = [ordered]@{
        mod_id = if ($e.PSObject.Properties.Name -contains 'mod_id' -and [int64]$e.mod_id -gt 0) { [int64]$e.mod_id } else { $mid }
        name = [string]$e.name; author = [string]$e.author; version = [string]$e.version
        unique_id = [string]$e.unique_id; description = [string]$e.description
        category_id = [int]($e.category_id); category = [string]$e.category; category_en = [string]$e.category_en
        file = [string]$e.file; size = [int64]$e.size
        _downloads = [int64]($e._downloads)
    }
    [void]$sync.Entries.Add($entry)
}
$todo = @($ids | Where-Object { -not $existing.ContainsKey([int]$_) })
Write-Host "already mirrored: $($existing.Count); to process: $($todo.Count)"

# ---------- runspace pool ----------
$iss = [System.Management.Automation.Runspaces.InitialSessionState]::CreateDefault()
$iss.ApartmentState = 'MTA'
$pool = [RunspaceFactory]::CreateRunspacePool(1, $Workers, $iss, $Host)
$pool.Open()
$jobs = @()
foreach ($id in $todo) {
    $ps = [PowerShell]::Create()
    $ps.RunspacePool = $pool
    [void]$ps.AddScript($worker).AddArgument($id).AddArgument($apiKey).AddArgument($apiBase).
        AddArgument($modsDir).AddArgument($sync).AddArgument($failedLog).AddArgument($MaxFileKb).AddArgument($gate).AddArgument($catMap)
    $jobs += [pscustomobject]@{ PS = $ps; Handle = $ps.BeginInvoke() }
}

# Snapshot the entry list under the shared gate, stripped to mirror schema.
function Get-Snapshot {
    [System.Threading.Monitor]::Enter($gate)
    try {
        @($sync.Entries | Sort-Object @{Expression={[int64]$_['_downloads']};Descending=$true} | ForEach-Object {
            [ordered]@{ mod_id=[int64]$_['mod_id']; name=$_['name']; author=$_['author']; version=$_['version'];
                        unique_id=$_['unique_id']; description=$_['description']; category_id=$_['category_id'];
                        category=$_['category']; category_en=$_['category_en']; file=$_['file'];
                        size=[int64]$_['size']; _downloads=[int64]$_['_downloads'] }
        })
    } finally { [System.Threading.Monitor]::Exit($gate) }
}

$lastWrite = 0
while ($jobs | Where-Object { -not $_.Handle.IsCompleted }) {
    Start-Sleep -Seconds 5
    $doneNow = $sync.Done + $sync.Failed + $sync.Skipped
    Write-Host ("progress: {0}/{1}  ok={2} fail={3} skip={4}  size={5:N1} MB" -f `
        $doneNow, ($todo.Count + $existing.Count), $sync.Done, $sync.Failed, $sync.Skipped, ($sync.Bytes/1MB))
    if ($sync.Done - $lastWrite -ge 10) {
        Write-Index (Get-Snapshot)
        $lastWrite = $sync.Done
    }
}
$pool.Close(); $pool.Dispose()

# final index, sorted by popularity
$final = Get-Snapshot
Write-Index $final
$totalMb = ($sync.Entries | ForEach-Object { [int64]$_['size'] } | Measure-Object -Sum).Sum / 1MB
Write-Host ("DONE: indexed={0} failed={1} skipped={2} total={3:N1} MB -> {4}" -f `
    $final.Count, $sync.Failed, $sync.Skipped, $totalMb, $indexPath)
