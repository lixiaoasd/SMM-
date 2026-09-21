# Verify that candidate mod ids map to the expected mod name via Nexus API.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$apiKey = (Get-Content (Join-Path $root 'nexus-api.key') -Raw).Trim()
$h = @{ apikey = $apiKey }

# id -> expected name fragment (case-insensitive substring match)
$cand = [ordered]@{
    1915 = 'Content Patcher'
    1348 = 'SpaceCore'
    5098 = 'Generic Mod Config'
    2400 = 'Harmony'
    6529 = 'Expanded Preconditions'
    3231 = 'Farm Type Manager'
    1720 = 'Json Assets'
    4970 = 'Producer Framework'
    8731 = 'Custom NPC Exclusions'
    2016 = 'PyTK'
    541  = 'Lookup Anything'
    239  = 'NPC Map Locations'
    518  = 'Chests Anywhere'
    1691 = 'Data Layers'
    1150 = 'UI Info Suite'
    1089 = 'Fast Animations'
    820  = 'AutoGate'
    533  = 'Skip Intro'
    492  = 'Billboard Anywhere'
    7500 = 'Horse Flute'
    279  = 'Loved Labels'
    16192 = 'Time Master'
    5096 = 'Experience Bars'
    4079 = 'Zoom Level'
    13619 = 'To-Dew'
    1307 = 'Rented Tools'
    1232 = 'Pony Weight Loss'
    3052 = 'Skull Cavern Elevator'
    8000 = 'Central Station'
    1845 = 'Bigger Backpack'
    1401 = 'Tractor'
    17767 = 'Better Sprinklers'
    1063 = 'Automate'
    4    = 'CJB Cheats'
    93   = 'CJB Item Spawner'
    87   = 'CJB Show Item Sell'
    3753 = 'Stardew Valley Expanded'
    7286 = 'Ridgeside'
    2007 = 'Magic'
    2571 = 'Deep Woods'
    2264 = 'Fishing Overhaul'
    2153 = 'Better Junimos'
}

foreach ($id in $cand.Keys) {
    try {
        $m = Invoke-RestMethod -Uri "https://api.nexusmods.com/v1/games/stardewvalley/mods/$id.json" -Headers $h -TimeoutSec 30
        $name = [string]$m.name
        $exp = $cand[$id]
        $ok = if ($name -match [regex]::Escape($exp)) { 'OK ' } else { 'MISMATCH' }
        $av = if ($m.available) { 'avail' } else { 'HIDDEN' }
        "{0,-8} {1} {2,-9} cat={3,-3} dl={4,-8} {5}" -f $id, $ok, $av, $m.category_id, $m.mod_downloads, $name
    } catch {
        "{0,-8} ERROR {1}" -f $id, $_.Exception.Message
    }
    Start-Sleep -Milliseconds 350
}
