# Sync a curated whitelist of famous SDV mods (frameworks / QoL / expansions / ...)
# that the weekly updated.json sweep never catches, and MERGE them into index.json.
# Same worker logic as sync-nexus.ps1; target ids come from a hardcoded whitelist.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File sync-extra.ps1 [-Workers 4]
param(
    [int]$Workers = 4
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

# Curated, API-verified whitelist (id => one-line note; notes are informational only).
$whitelist = [ordered]@{
    # ---- frameworks (SMAPI 2400 intentionally excluded: installer zip has no manifest) ----
    1915  = 'Content Patcher'
    1348  = 'SpaceCore'
    5098  = 'Generic Mod Config Menu'
    6529  = 'Expanded Preconditions Utility'
    3231  = 'Farm Type Manager'
    1720  = 'Json Assets'
    4970  = 'Producer Framework Mod'
    1726  = 'PyTK'
    7089  = 'Custom NPC Exclusions'
    10747 = 'SAAT'
    5005  = 'Shop Tile Framework'
    9633  = 'Extra Map Layers'
    5371  = 'Anti-Social NPCs'
    8626  = 'Custom Companions'
    1536  = 'Mail Framework Mod'
    3213  = 'StardewHack'
    4736  = "DaisyNiko's Tilesheets"
    # ---- quality of life ----
    541   = 'Lookup Anything'
    239   = 'NPC Map Locations'
    518   = 'Chests Anywhere'
    1691  = 'Data Layers'
    7098  = 'UI Info Suite 2'
    1089  = 'Fast Animations'
    3109  = 'Automatic Gates'
    533   = 'Skip Intro'
    492   = 'Billboard Anywhere'
    7500  = 'Horse Flute Anywhere'
    279   = 'Loved Labels'
    16192 = 'Time Master'
    7363  = 'Zoom Level'
    7495  = 'Show Birthdays'
    8000  = 'Central Station'
    6183  = 'Train Station'
    21264 = 'BusLocations Continued (1.6)'
    19675 = 'Happy Home Designer'
    7409  = 'To-Dew'
    5     = 'CJB Show Item Sell Price'
    1845  = 'Bigger Backpack'
    # ---- automation ----
    1063  = 'Automate'
    1401  = 'Tractor Mod'
    17767 = 'Better Sprinklers Plus'
    7920  = 'Deluxe Grabber Redux'
    2731  = 'Yet Another Harvest With Scythe'
    963   = 'Skull Cavern Elevator'
    1601  = 'Winter Grass'
    859   = 'Better Ranching'
    # ---- big expansions ----
    3753  = 'Stardew Valley Expanded'
    7286  = 'Ridgeside Village'
    5787  = 'East Scarp'
    8587  = 'Downtown Zuzu'
    6372  = 'Stardew Aquarium'
    # ---- cheats ----
    4     = 'CJB Cheats Menu'
    93    = 'CJB Item Spawner'
    # ---- gameplay / content ----
    2007  = 'Magic'
    2571  = 'DeepWoods'
    2697  = 'Skip Fishing Minigame'
    8970  = 'Fishing Info Overlays'
    8897  = 'Visible Fish'
    4542  = 'Medieval buildings'
    5969  = 'Seasonal Outfits - SVE'
    5450  = 'Seasonal Outfits - Cuter'
    2544  = 'Canon-Friendly Dialogue Expansion'
    5662  = 'Animated Furniture and Stuff'
    5735  = 'Animated Fish'
    6830  = 'The Love of Cooking'
    9969  = 'Fashion Sense'
    # ---- translations for the big expansions ----
    10342 = 'SVE Chinese'
    9247  = 'Ridgeside Chinese'
}

# category_id -> @{ en; zh } table
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
$failedLog = Join-Path $root 'sync-extra-failed.log'
if (Test-Path $failedLog) { Remove-Item $failedLog -Force }

function Write-Index {
    param($List)
    if ($List.Count -eq 0) { $json = '[]' }
    else { $json = ConvertTo-Json -InputObject @($List) -Depth 4 }
    [IO.File]::WriteAllText($indexPath, $json, (New-Object Text.UTF8Encoding($false)))
}

# ---------- worker (self-contained; PS5 runspaces) ----------
$worker = {
    param($modId, $apiKey, $apiBase, $modsDir, $sync, $failedLog, $gate, $CatMap)

    $ProgressPreference = 'SilentlyContinue'

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
                    Start-Sleep -Seconds ([Math]::Min($retryAfter, 1800) + 1)
                    continue
                }
                if ($code -ge 500) { Start-Sleep -Seconds 5; continue }
                throw
            }
        }
        throw "rate-limit retries exhausted"
    }

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

        $fname = "n$modId-$fid.zip"
        $fpath = Join-Path $modsDir $fname
        $needDownload = $true
        if ((Test-Path $fpath) -and (Get-Item $fpath).Length -gt 0) { $needDownload = $false }

        if ($needDownload) {
            # 3) premium CDN link
            $links = Invoke-Nexus "$apiBase/mods/$modId/files/$fid/download_link.json?key=premium" $h
            if (-not $links -or $links.Count -eq 0 -or -not $links[0].URI) { throw "no cdn link" }
            $tmp = "$fpath.part"
            Invoke-WebRequest -Uri $links[0].URI -OutFile $tmp -UseBasicParsing -TimeoutSec 1800
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

# ---------- merge with existing index (read as UTF-8!) ----------
$existing = @{}
if (Test-Path $indexPath) {
    try {
        $old = [IO.File]::ReadAllText($indexPath) | ConvertFrom-Json
        foreach ($e in @($old)) {
            $mt = [regex]::Match([string]$e.file, 'n(\d+)-\d+\.zip$')
            if ($mt.Success) {
                $mid = [int]$mt.Groups[1].Value
                $zp = Join-Path $root ("public\" + ([string]$e.file -replace '/', '\'))
                if (Test-Path $zp) {
                    $existing[$mid] = [ordered]@{
                        mod_id      = if ($e.PSObject.Properties.Name -contains 'mod_id' -and [int64]$e.mod_id -gt 0) { [int64]$e.mod_id } else { $mid }
                        name        = [string]$e.name
                        author      = [string]$e.author
                        version     = [string]$e.version
                        unique_id   = [string]$e.unique_id
                        description = [string]$e.description
                        category_id = [int]($e.category_id)
                        category    = [string]$e.category
                        category_en  = [string]$e.category_en
                        file        = [string]$e.file
                        size        = [int64]$e.size
                        _downloads  = [int64]($e._downloads)
                    }
                }
            }
        }
    } catch { Write-Host "WARN: could not parse existing index: $($_.Exception.Message)" }
}
foreach ($kv in $existing.GetEnumerator()) { [void]$sync.Entries.Add($kv.Value) }
Write-Host "existing mirrored: $($existing.Count)"

$todo = @($whitelist.Keys | Where-Object { -not $existing.ContainsKey([int]$_) } | ForEach-Object { [int]$_ })
Write-Host "whitelist: $($whitelist.Count); to fetch: $($todo.Count)"

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
        AddArgument($modsDir).AddArgument($sync).AddArgument($failedLog).AddArgument($gate).AddArgument($catMap)
    $jobs += [pscustomobject]@{ PS = $ps; Handle = $ps.BeginInvoke() }
}

function Get-Snapshot {
    [System.Threading.Monitor]::Enter($gate)
    try {
        @($sync.Entries | Sort-Object @{Expression = { [int64]$_['_downloads'] }; Descending = $true} | ForEach-Object {
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
        $doneNow, $todo.Count, $sync.Done, $sync.Failed, $sync.Skipped, ($sync.Bytes/1MB))
    if ($sync.Done - $lastWrite -ge 5) {
        Write-Index (Get-Snapshot)
        $lastWrite = $sync.Done
    }
}
$pool.Close(); $pool.Dispose()

# rethrow worker errors for visibility
$wi = 0
foreach ($j in $jobs) {
    if ($j.Handle.IsCompleted) {
        try { $j.PS.EndInvoke($j.Handle) | Out-Null } catch { Write-Host "worker error: $_" }
        $j.PS.Dispose()
    }
    $wi++
}

$final = Get-Snapshot
Write-Index $final
$totalMb = ($final | ForEach-Object { [int64]$_.size } | Measure-Object -Sum).Sum / 1MB
Write-Host ("DONE: indexed={0} (+{1} new) failed={2} skipped={3} total={4:N1} MB -> {5}" -f `
    $final.Count, $sync.Done, $sync.Failed, $sync.Skipped, $totalMb, $indexPath)
if ($sync.Failed -gt 0) { Write-Host "failures logged in $failedLog" }
