$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$exe = @(
    'speaky-gpui.exe'
    'speaky-gpui/target/release/speaky-gpui.exe'
    'speaky-gpui/target/debug/speaky-gpui.exe'
) | ForEach-Object { Join-Path $root $_ } |
    Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
    Select-Object -First 1
if (!$exe) { throw 'Build Speaky first or extract the Windows release ZIP.' }
$mutex = [Threading.Mutex]::new($false, 'Local\SpeakyWindowsLauncher')
try {
    try { $started = $mutex.WaitOne(0) }
    catch [Threading.AbandonedMutexException] { $started = $true }
    if ($started) {
        try { Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe -Parent) -Wait }
        finally { $mutex.ReleaseMutex() }
    }
} finally {
    $mutex.Dispose()
}
