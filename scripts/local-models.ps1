param(
    [Parameter(Mandatory=$true)][ValidateSet('List','Ensure','Release')][string]$Action,
    [string]$ModelId,
    [int]$OwnerPid
)
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$root = Split-Path $PSScriptRoot -Parent
$pythonPathFile = @(
    (Join-Path $root 'models/asr-python-path.txt')
    (Join-Path (Split-Path $root -Parent) 'models/asr-python-path.txt')
) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
$savedPython = if ($pythonPathFile) {
    (Get-Content -LiteralPath $pythonPathFile -Raw -Encoding UTF8).Trim()
}
$python = @(
    $env:SPEAKY_QWEN_PYTHON
    $savedPython
    (Join-Path $root 'models/asr-env/Scripts/python.exe')
    (Join-Path (Split-Path $root -Parent) 'speaky/models/asr-env/Scripts/python.exe')
) | Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) } | Select-Object -First 1
if (!$python) { $python = (Get-Command python.exe -ErrorAction SilentlyContinue | Select-Object -First 1).Source }
if (!$python -or $python -match '\\Microsoft\\WindowsApps\\python.exe$') {
    if ($Action -eq 'List') { Write-Output '[]'; exit 0 }
    if ($Action -eq 'Release') {
        $profileDir = if ($env:SPEAKY_CONFIG_DIR) { $env:SPEAKY_CONFIG_DIR } else { Join-Path $env:APPDATA 'speaky/gpui' }
        if (!(Test-Path -LiteralPath (Join-Path $profileDir "local-asr-$OwnerPid.json"))) {
            Write-Output '{"released":true}'
            exit 0
        }
    }
    throw 'Set SPEAKY_QWEN_PYTHON to the Python environment used by the local speech models.'
}
$arguments = @('-B', '-X', 'utf8', (Join-Path $PSScriptRoot 'local_models.py'), $Action)
if ($Action -eq 'Ensure') { $arguments += @('--model', $ModelId, '--owner', $OwnerPid) }
if ($Action -eq 'Release') { $arguments += @('--owner', $OwnerPid) }
& $python @arguments
exit $LASTEXITCODE
