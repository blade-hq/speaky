# Launch a provisioned local CPU recognizer alongside the Windows client.
[CmdletBinding()]
param([ValidateSet('sherpa-onnx','vosk','speechbrain','wenet','espnet','nemo','funasr','kaldi')][string]$Framework)
$ErrorActionPreference = 'Stop'
$appRoot = Split-Path $PSScriptRoot -Parent
$roots = @($env:SPEAKY_MODELS_DIR -split ';' | Where-Object { $_ })
$roots += (Join-Path $appRoot 'ASR-Models'), (Join-Path (Split-Path $appRoot -Parent) 'ASR-Models')
function Find-Asset([string]$relative) {
    foreach ($root in $roots) {
        $candidate = Join-Path $root $relative
        if (Test-Path -LiteralPath $candidate) { return (Resolve-Path -LiteralPath $candidate).Path }
    }
}
$presets = @(Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $PSScriptRoot 'asr-presets.json') | ConvertFrom-Json | ForEach-Object { $_ })
$profiles = [ordered]@{}
foreach ($preset in $presets) { $profiles[$preset.framework] = $preset }
$kaldiBin = Find-Asset 'kaldi-env\Library\bin'
if (!$Framework) {
    $ids = @($profiles.Keys)
    for ($i = 0; $i -lt $ids.Count; $i++) {
        $id = $ids[$i]
        $preset = $profiles[$id]
        $available = (Find-Asset "$($preset.environment)\Scripts\python.exe") -and (Find-Asset $preset.directory)
        if ($id -eq 'kaldi' -and !$kaldiBin -and !(Get-Command 'online2-wav-nnet3-latgen-faster' -ErrorAction SilentlyContinue)) { $available = $false }
        $state = if ($available) { '' } else { ' [需配置环境或模型]' }
        Write-Host ("{0}. {1}  ({2}){3}" -f ($i+1), $id, $preset.model, $state)
    }
    $answer = Read-Host '输入要启动的识别框架编号 [1-8]'
    if ($answer -notmatch '^[1-8]$') { throw '请输入 1 到 8 之间的编号。' }
    $Framework = $ids[([int]$answer) - 1]
}
$preset = $profiles[$Framework]
$python = Find-Asset "$($preset.environment)\Scripts\python.exe"
$model = Find-Asset $preset.directory
if (!$python -or !$model) {
    throw "$Framework 需要 ASR-Models/$($preset.environment) 和 ASR-Models/$($preset.directory)。配置说明见 docs/ASR-frameworks.md。"
}
if ($Framework -eq 'kaldi' -and $kaldiBin) {
    $kaldiPrefix = Split-Path (Split-Path $kaldiBin -Parent) -Parent
    $env:Path = "$kaldiBin;$kaldiPrefix;$env:Path"
}
$env:PYTHONUTF8 = '1'
$options = @()
if (Test-Path -LiteralPath (Join-Path $model 'speaky-options.json') -PathType Leaf) {
    $options = @('--options', (Join-Path $model 'speaky-options.json'))
}
Write-Host "正在启动 $Framework / $($preset.model)。听写时请保持本窗口打开，按 Ctrl+C 停止服务。"
& $python (Join-Path $PSScriptRoot 'asr_bridge.py') --framework $Framework --model-dir $model --model-id $preset.model --model-alias $Framework @options
exit $LASTEXITCODE
