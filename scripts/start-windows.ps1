# Launch the Windows client. The GUI owns its selected GPU model.
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$crate = $root
$target = if ($env:CARGO_TARGET_DIR) {
    if ([IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR)) { $env:CARGO_TARGET_DIR }
    else { Join-Path $crate $env:CARGO_TARGET_DIR }
} else { Join-Path $crate 'target' }

$executables = foreach ($subdir in @('', 'x86_64-pc-windows-gnu', 'x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc')) {
    foreach ($kind in @('release', 'debug')) {
        $candidate = Join-Path $target "$subdir\$kind\speaky-gpui.exe".TrimStart('\')
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { Get-Item -LiteralPath $candidate }
    }
}
$packagedExe = Join-Path $root 'speaky-gpui.exe'
$exe = if (Test-Path -LiteralPath $packagedExe -PathType Leaf) {
    Get-Item -LiteralPath $packagedExe
} else { $executables | Sort-Object LastWriteTime -Descending | Select-Object -First 1 }
if (!$exe) { throw "Build the Windows client first: cargo build --locked --manifest-path `"$crate\Cargo.toml`"" }

$profileDir = if ($env:SPEAKY_CONFIG_DIR) { $env:SPEAKY_CONFIG_DIR } else {
    Join-Path ([Environment]::GetFolderPath('ApplicationData')) 'speaky\gpui'
}
$profileDir = [IO.Path]::GetFullPath($profileDir)
$env:SPEAKY_CONFIG_DIR = $profileDir
$env:NO_PROXY = "127.0.0.1,localhost,$env:NO_PROXY".TrimEnd(',')
$config = Join-Path $profileDir 'config.yaml'
$modelId = 'Qwen3-ASR-0.6B'

function Show-RunningApp($lockPath) {
    $stream = [IO.File]::Open($lockPath, 'Open', 'Read', 'ReadWrite')
    try {
        $reader = New-Object IO.StreamReader($stream)
        try { $processIdText = $reader.ReadToEnd() } finally { $reader.Dispose() }
    } finally { $stream.Dispose() }
    if ($processIdText -notmatch '^\d+$') { return } # Another launch is starting the model.
    $running = Get-Process -Id ([int]$processIdText) -ErrorAction SilentlyContinue
    if (!$running -or $running.ProcessName -ne 'speaky-gpui') { return }
    Add-Type -Name AppWindow -Namespace Speaky -MemberDefinition @'
public delegate bool EnumCallback(IntPtr hwnd, IntPtr data);
[System.Runtime.InteropServices.DllImport("user32.dll")] static extern bool EnumWindows(EnumCallback callback, IntPtr data);
[System.Runtime.InteropServices.DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
[System.Runtime.InteropServices.DllImport("user32.dll", CharSet=System.Runtime.InteropServices.CharSet.Unicode)]
static extern int GetWindowText(IntPtr hwnd, System.Text.StringBuilder title, int capacity);
public static IntPtr FindMain(uint pid) {
    IntPtr main = IntPtr.Zero;
    EnumWindows((hwnd, data) => {
        uint owner;
        GetWindowThreadProcessId(hwnd, out owner);
        if (owner != pid) return true;
        var title = new System.Text.StringBuilder(32);
        GetWindowText(hwnd, title, title.Capacity);
        if (title.ToString() != "Speaky") return true;
        main = hwnd;
        return false;
    }, IntPtr.Zero);
    return main;
}
[System.Runtime.InteropServices.DllImport("user32.dll")] public static extern bool IsIconic(IntPtr window);
[System.Runtime.InteropServices.DllImport("user32.dll")] public static extern bool ShowWindowAsync(IntPtr window, int command);
[System.Runtime.InteropServices.DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
'@
    $main = [Speaky.AppWindow]::FindMain($running.Id)
    if ($main -eq [IntPtr]::Zero) { return }
    if ([Speaky.AppWindow]::IsIconic($main)) {
        [void][Speaky.AppWindow]::ShowWindowAsync($main, 9)
    }
    [void][Speaky.AppWindow]::SetForegroundWindow($main)
}

[void][IO.Directory]::CreateDirectory($profileDir)
$lockPath = Join-Path $profileDir 'windows-launcher.lock'
try { $launchLock = [IO.File]::Open($lockPath, 'OpenOrCreate', 'ReadWrite', 'Read') }
catch [IO.IOException] {
    if (($_.Exception.HResult -band 0xffff) -notin @(32, 33)) { throw }
    Show-RunningApp $lockPath
    exit 0
}
$launchLock.SetLength(0)

function Install-InitialConfig([string]$text) {
    $temporary = Join-Path $profileDir ('.initial-' + [guid]::NewGuid().ToString() + '.tmp')
    try {
        [IO.File]::WriteAllText($temporary, $text, [Text.UTF8Encoding]::new($false))
        try { [IO.File]::Move($temporary, $config) }
        catch [IO.IOException] { if (!(Test-Path -LiteralPath $config)) { throw } }
    } finally { [IO.File]::Delete($temporary) }
}
if (!(Test-Path -LiteralPath $config)) {
    try {
        $catalog = & (Join-Path $PSScriptRoot 'local-models.ps1') -Action List
        if ($LASTEXITCODE -eq 0) {
            $localModels = @($catalog | ConvertFrom-Json | ForEach-Object { $_ } | Where-Object { $_.id })
            $initial = $localModels | Where-Object id -eq $modelId | Select-Object -First 1
            if (!$initial) { $initial = $localModels | Select-Object -First 1 }
            if ($initial) {
                Install-InitialConfig "engine:`n  current: local`n  local:`n    model: $($initial.id)`n"
            }
        }
    } catch { Write-Warning "Could not initialize the local model selection: $_" }
}
try {
    $app = Start-Process -FilePath $exe.FullName -WorkingDirectory $root -PassThru
    $pidBytes = [Text.Encoding]::ASCII.GetBytes($app.Id.ToString())
    $launchLock.Write($pidBytes, 0, $pidBytes.Length)
    $launchLock.Flush()
    $app.WaitForExit()
    if ($app.ExitCode) { throw "Speaky exited with code $($app.ExitCode)." }
} finally {
    $launchLock.Dispose()
}
