# Download confirmed popular SDV mod release zips into public/mods via local proxy.
$ErrorActionPreference = 'Stop'
$proxy = 'http://127.0.0.1:7897'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$dest = Join-Path $root 'public\mods'
if (-not (Test-Path $dest)) { New-Item -ItemType Directory -Path $dest | Out-Null }

$mods = @(
    'https://github.com/Annosz/UIInfoSuite2/releases/download/v2.3.7/UIInfoSuite2.v2.3.7.zip',
    'https://github.com/Esca-MMC/FarmTypeManager/releases/download/1.26.1/FarmTypeManager.1.26.1.zip',
    'https://github.com/urbanyeti/stardew-better-ranching/releases/download/2.0.5/BetterRanching.2.0.5.zip',
    'https://github.com/LeFauxMatt/CarryChests/releases/download/v1.3.0/CarryChests.1.3.0.zip',
    'https://github.com/LeFauxMatt/ExpandedStorage/releases/download/v3.3.0/ExpandedStorage.3.3.0.zip',
    'https://github.com/LeFauxMatt/ColorfulChests/releases/download/v1.1.0/ColorfulChests.1.1.0.zip'
)
foreach ($u in $mods) {
    $name = Split-Path $u -Leaf
    $out = Join-Path $dest $name
    Write-Host "downloading $name ..."
    & curl.exe -sL --proxy $proxy --retry 2 --connect-timeout 20 -o $out $u
    if (Test-Path $out) { Write-Host ("  ok {0:N0} bytes" -f (Get-Item $out).Length) }
    else { Write-Host "  FAILED $u" }
}
