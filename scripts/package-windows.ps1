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
    'scripts/start-windows.ps1' = Join-Path $PSScriptRoot 'start-windows.ps1'
    'config.example.yaml' = Join-Path $root 'config.example.yaml'
}
$guide = Get-Item (Join-Path $root 'docs/Windows*.md')
$files["docs/$($guide.Name)"] = $guide.FullName
# Bundle redistributable MSVC runtime files when available from Build Tools.
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
$crt = if ($env:VCToolsRedistDir) { $env:VCToolsRedistDir } elseif (Test-Path -LiteralPath $vswhere) {
    $vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($vs) {
        Get-ChildItem (Join-Path $vs 'VC/Redist/MSVC') -Directory -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '^\d+(\.\d+)+$' } |
            Sort-Object { [version]$_.Name } -Descending |
            Select-Object -First 1 -ExpandProperty FullName
    }
}
if ($crt) {
    Get-ChildItem (Join-Path $crt 'x64/Microsoft.VC*.CRT/*.dll') -ErrorAction SilentlyContinue |
        ForEach-Object { $files[$_.Name] = $_.FullName }
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
