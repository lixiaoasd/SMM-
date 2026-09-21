# Round 2: verify extra mod ids.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$apiKey = (Get-Content (Join-Path $root 'nexus-api.key') -Raw).Trim()
$h = @{ apikey = $apiKey }

$cand = [ordered]@{
    5787  = 'East Scarp'
    8587  = 'Downtown Zuzu'
    7098  = 'UI Info Suite'
    7089  = 'Custom NPC Exclusions'
    1726  = 'PyTK'
    10747 = 'SAAT'
    5005  = 'Shop Tile Framework'
    9633  = 'Extra Map Layers'
    5371  = 'AntiSocial'
    8626  = 'Custom Companions'
    1536  = 'Mail Framework'
    3109  = 'Automatic'
    4542  = 'Medieval'
    2697  = 'Skip Fishing'
    7920  = 'Grabber'
    2731  = 'Scythe'
    963   = 'Skull'
    1958  = 'Passable Crops'
    7363  = 'Zoom'
    7495  = 'Birthday'
    1601  = 'Winter Grass'
    5969  = 'Seasonal Outfits'
    5450  = 'Seasonal Cute'
    2544  = 'Canon-Friendly Dialogue'
    8970  = 'Fishing Info'
    859   = 'Better Ranching'
    6372  = 'Aquarium'
    2221  = 'Better Junimos'
    5     = 'CJB Show Item Sell'
    4736  = 'DaisyNiko'
    10342 = 'Stardew Valley Expanded'
    9247  = 'Ridgeside'
    3213  = 'Harmony'
    5662  = 'Animated Furniture'
    5735  = 'Animated Fish'
    4106  = "Grandpa's Farm"
    4222  = 'Immersive Farm'
    5539  = 'Noclip'
    11345 = 'Lunna'
    7336  = 'Walk to the Desert'
    19675 = 'Happy Home'
    4856  = 'Experience Bars'
}

foreach ($id in $cand.Keys) {
    try {
        $m = Invoke-RestMethod -Uri "https://api.nexusmods.com/v1/games/stardewvalley/mods/$id.json" -Headers $h -TimeoutSec 30
        $name = [string]$m.name
        $exp = $cand[$id]
        $ok = if ($name -match [regex]::Escape($exp)) { 'OK ' } else { 'MISMATCH' }
        $av = if ($m.available) { 'avail' } else { 'HIDDEN' }
        "{0,-8} {1} {2,-7} cat={3,-3} dl={4,-9} {5}" -f $id, $ok, $av, $m.category_id, $m.mod_downloads, $name
    } catch {
        "{0,-8} ERROR {1}" -f $id, $_.Exception.Message
    }
    Start-Sleep -Milliseconds 300
}
