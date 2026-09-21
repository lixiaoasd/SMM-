# Generate a tiny sample mod zip (with manifest.json) into public/mods for end-to-end testing.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$mods = Join-Path $root 'public\mods'
if (-not (Test-Path $mods)) { New-Item -ItemType Directory -Path $mods | Out-Null }
$zipPath = Join-Path $mods 'stardew-sample-mod-1.0.0.zip'
if (Test-Path $zipPath) { Remove-Item $zipPath -Force }

$manifest = @'
{
  "Name": "StardewModManager Sample Mod",
  "Author": "StardewModManager",
  "Version": "1.0.0",
  "Description": "Local mirror end-to-end test package.",
  "UniqueID": "StardewModManager.SampleMod",
  "EntryDll": "SampleMod.dll"
}
'@

$zip = [IO.Compression.ZipFile]::Open($zipPath, [IO.Compression.ZipArchiveMode]::Create)
try {
    function Add-Entry($zip, $path, $content) {
        $e = $zip.CreateEntry($path)
        $w = New-Object IO.StreamWriter($e.Open())
        try { $w.Write($content) } finally { $w.Close() }
    }
    Add-Entry $zip 'StardewModManagerSampleMod/manifest.json' $manifest
    Add-Entry $zip 'StardewModManagerSampleMod/SampleMod.dll' 'dummy dll content for mirror download test'
} finally { $zip.Dispose() }
Write-Host "created: $zipPath"
