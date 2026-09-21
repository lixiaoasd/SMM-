# Round 3: verify remaining candidate ids + cross-check trending list.
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$apiKey = (Get-Content (Join-Path $root 'nexus-api.key') -Raw).Trim()
$h = @{ apikey = $apiKey }
$apiBase = 'https://api.nexusmods.com/v1/games/stardewvalley'

function Invoke-Nexus($url) {
    for ($attempt = 0; $attempt -lt 20; $attempt++) {
        try { return Invoke-RestMethod -Uri $url -Headers $h -TimeoutSec 45 }
        catch {
            $code = 0
            if ($_.Exception.Response) { $code = [int]$_.Exception.Response.StatusCode }
            if ($code -eq 429 -or $code -eq 503) { Start-Sleep -Seconds 20; continue }
            throw
        }
    }
    throw "rate-limit retries exhausted"
}

$cand = [ordered]@{
    6183  = 'Train Station'
    2905  = 'Bus'
    21264 = 'Bus'
    7409  = 'To-Dew'
    8897  = 'Visible Fish'
    6830  = 'Cooking'
    9969  = 'Fashion'
    15980 = 'East Scarp'
    1845  = 'Backpack'
    5539  = 'Noclip'
    6797  = 'Debug'
    1950  = 'Japanese'
}

Write-Host "=== explicit candidates ==="
$verified = @()
foreach ($id in $cand.Keys) {
    try {
        $m = Invoke-Nexus "$apiBase/mods/$id.json"
        $name = [string]$m.name
        $exp = $cand[$id]
        $ok = if ($name -match [regex]::Escape($exp)) { 'OK ' } else { 'MISMATCH' }
        $av = if ($m.available) { 'avail' } else { 'HIDDEN' }
        "{0,-8} {1} {2,-7} cat={3,-3} dl={4,-9} {5}" -f $id, $ok, $av, $m.category_id, $m.mod_downloads, $name
        if ($ok -eq 'OK ' -and $av -eq 'avail') { $verified += [int]$id }
    } catch {
        "{0,-8} ERROR {1}" -f $id, $_.Exception.Message
    }
    Start-Sleep -Milliseconds 400
}

Write-Host ""
Write-Host "=== trending missing from library ==="
$idx = @{}
$j = Get-Content (Join-Path $root 'public\index.json') -Raw | ConvertFrom-Json
foreach ($e in @($j)) { if ($e.file -match 'n(\d+)-') { $idx[[int]$Matches[1]] = $true } }

$tr = Invoke-Nexus "$apiBase/mods/trending.json"
$n = 0
foreach ($m in @($tr)) {
    $id = [int]$m.mod_id
    if (-not $idx.ContainsKey($id)) {
        "{0,-8} cat={1,-3} dl={2,-9} {3}" -f $id, $m.category_id, $m.mod_downloads, $m.name
        $n++
    }
}
Write-Host "trending missing: $n (of $($tr.Count))"
