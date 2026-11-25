param(
    [Parameter(Mandatory = $true)][string]$Executable,
    [Parameter(Mandatory = $true)][string]$Output,
    [string]$WhisperRuntime
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
    'scripts/share-windows-models.py' = Join-Path $PSScriptRoot 'share-windows-models.py'
    'scripts/shared_windows_models.py' = Join-Path $PSScriptRoot 'shared_windows_models.py'
    'scripts/gpu_worker.py' = Join-Path $PSScriptRoot 'gpu_worker.py'
    'scripts/sense_gpu.py' = Join-Path $PSScriptRoot 'sense_gpu.py'
    'scripts/whisper_gpu.py' = Join-Path $PSScriptRoot 'whisper_gpu.py'
    'scripts/whisper_gpu_bridge.dll' = Join-Path $PSScriptRoot 'whisper_gpu_bridge.dll'
    'docs/Win7部署说明.md' = Join-Path $root 'docs/Win7部署说明.md'
}
if ($WhisperRuntime) {
    foreach ($name in @('whisper-server.exe', 'LICENSE')) {
        $runtimeFile = Get-Item -LiteralPath (Join-Path $WhisperRuntime $name)
        $files["whisper-runtime/$name"] = $runtimeFile.FullName
    }
}
$guide = Get-Item (Join-Path $root 'docs/Windows*.md')
$files["docs/$($guide.Name)"] = $guide.FullName
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
