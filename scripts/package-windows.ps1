param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [Parameter(Mandatory = $true)][string]$Output
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$root = Split-Path $PSScriptRoot -Parent
$exe = Get-Item -LiteralPath $Executable
$files = [ordered]@{
    'speaky-gpui.exe' = $exe.FullName
    'start-windows.cmd' = Join-Path $root 'start-windows.cmd'
    'start-asr.cmd' = Join-Path $root 'start-asr.cmd'
    'scripts/start-asr.ps1' = Join-Path $PSScriptRoot 'start-asr.ps1'
    'scripts/asr-presets.json' = Join-Path $PSScriptRoot 'asr-presets.json'
    'scripts/asr_bridge.py' = Join-Path $PSScriptRoot 'asr_bridge.py'
    'scripts/asr_adapters_light.py' = Join-Path $PSScriptRoot 'asr_adapters_light.py'
    'scripts/asr_adapters_research.py' = Join-Path $PSScriptRoot 'asr_adapters_research.py'
    'docs/ASR-frameworks.md' = Join-Path $root 'docs/ASR-frameworks.md'
    'scripts/start-windows.ps1' = Join-Path $PSScriptRoot 'start-windows.ps1'
    'scripts/serve_qwen_asr.py' = Join-Path $PSScriptRoot 'serve_qwen_asr.py'
    'scripts/local-models.ps1' = Join-Path $PSScriptRoot 'local-models.ps1'
    'scripts/local_models.py' = Join-Path $PSScriptRoot 'local_models.py'
    'scripts/gpu_worker.py' = Join-Path $PSScriptRoot 'gpu_worker.py'
    'scripts/sense_gpu.py' = Join-Path $PSScriptRoot 'sense_gpu.py'
    'scripts/whisper_gpu.py' = Join-Path $PSScriptRoot 'whisper_gpu.py'
}
$bridge = Join-Path $PSScriptRoot 'whisper_gpu_bridge.dll'
if (Test-Path -LiteralPath $bridge -PathType Leaf) {
    $files['scripts/whisper_gpu_bridge.dll'] = $bridge
}
$guide = Get-Item (Join-Path $root 'docs/Windows*.md')
$files["docs/$($guide.Name)"] = $guide.FullName
$datasets = Join-Path $root 'ASR-Tests'
$sets = if (Test-Path -LiteralPath $datasets -PathType Container) {
    Get-ChildItem -LiteralPath $datasets -Directory
}
foreach ($set in $sets) {
    $manifest = Join-Path $set.FullName 'manifest.json'
    if (!(Test-Path -LiteralPath $manifest -PathType Leaf)) { continue }
    $prefix = "ASR-Tests/$($set.Name)"
    $files["$prefix/manifest.json"] = $manifest
    $dataset = Get-Content -LiteralPath $manifest -Raw -Encoding UTF8 | ConvertFrom-Json
    foreach ($case in $dataset.cases) {
        $wav = [IO.Path]::GetFullPath((Join-Path $set.FullName $case.audio))
        if (!$wav.StartsWith(($set.FullName + '\'), [StringComparison]::OrdinalIgnoreCase) -or
            [IO.Path]::GetExtension($wav) -ne '.wav') { throw "Invalid sample in $manifest" }
        $relative = $wav.Substring($set.FullName.Length + 1).Replace('\','/')
        $files["$prefix/$relative"] = $wav
    }
}
# GNU builds can place their required runtime libraries beside the executable.
foreach ($name in @('libgcc_s_seh-1.dll', 'libstdc++-6.dll', 'libwinpthread-1.dll')) {
    $runtime = Join-Path $exe.DirectoryName $name
    if (Test-Path -LiteralPath $runtime) { $files[$name] = $runtime }
}
$dest = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Output)
$archive = [IO.Compression.ZipFile]::Open($dest, [IO.Compression.ZipArchiveMode]::Create)
try {
    foreach ($entry in $files.GetEnumerator()) {
        [void][IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
            $archive, $entry.Value, $entry.Key, [IO.Compression.CompressionLevel]::Optimal)
    }
} finally {
    $archive.Dispose()
}
Write-Output $dest
