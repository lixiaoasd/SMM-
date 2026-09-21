# Attach category names (zh/en) to every index.json entry using categories.json.
# Keeps category_id and _downloads; preserves popularity order. Idempotent.
$ErrorActionPreference = 'Stop'
$root      = Split-Path -Parent $MyInvocation.MyCommand.Path
$indexPath = Join-Path $root 'public\index.json'
$catPath   = Join-Path $root 'categories.json'

$p = [IO.File]::ReadAllText($indexPath) | ConvertFrom-Json
$catRaw = [IO.File]::ReadAllText($catPath) | ConvertFrom-Json
$nameOf = @{}
foreach ($prop in $catRaw.PSObject.Properties) {
    $nameOf[[int]$prop.Name] = $prop.Value
}

$out = foreach ($e in $p) {
    $cid = [int]$e.category_id
    $zh = ''; $en = ''
    if ($nameOf.ContainsKey($cid)) {
        $zh = [string]$nameOf[$cid].zh
        $en = [string]$nameOf[$cid].en
    }
    $mid = [int64]0
    if ($e.PSObject.Properties.Name -contains 'mod_id') { $mid = [int64]$e.mod_id }
    if ($mid -le 0) {
        $mt = [regex]::Match([string]$e.file, 'n(\d+)-\d+\.zip$')
        if ($mt.Success) { $mid = [int64]$mt.Groups[1].Value }
    }
    [ordered]@{
        mod_id      = $mid
        name        = [string]$e.name
        author      = [string]$e.author
        version     = [string]$e.version
        unique_id   = [string]$e.unique_id
        description = [string]$e.description
        category_id = $cid
        category    = $zh
        category_en = $en
        file        = [string]$e.file
        size        = [int64]$e.size
        _downloads  = [int64]$e._downloads
    }
}
$final = @($out | Sort-Object @{Expression={ [int64]$_._downloads }; Descending=$true})
$json = ConvertTo-Json -InputObject $final -Depth 4
[IO.File]::WriteAllText($indexPath, $json, (New-Object Text.UTF8Encoding($false)))

# verify
$check = [IO.File]::ReadAllText($indexPath) | ConvertFrom-Json
"DONE: entries=$($check.Count) withCategory=$(@($check | Where-Object { $_.category }).Count)"
$check | Where-Object { -not $_.category } | Select-Object -First 5 | ForEach-Object { "  uncategorized: $($_.file) cat=$($_.category_id)" }
