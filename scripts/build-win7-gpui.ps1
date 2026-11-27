[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $ToolRoot,
    [string] $BuildDirectory,
    [string] $TargetDirectory,
    [string] $Fxc,
    [string] $GpuiCrate,
    [switch] $Offline
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$target = 'x86_64-win7-windows-gnu'
$toolchain = 'stable-x86_64-pc-windows-gnu'
$gpuiSha256 = '979b45cfa6ec723b6f42330915a1b3769b930d02b2d505f9697f8ca602bee707'
if (!$BuildDirectory) { $BuildDirectory = Join-Path $root 'target\win7-gpui' }
$BuildDirectory = [IO.Path]::GetFullPath($BuildDirectory)
if (!$TargetDirectory) { $TargetDirectory = Join-Path $BuildDirectory 'cargo' }
$TargetDirectory = [IO.Path]::GetFullPath($TargetDirectory)
$ToolRoot = (Resolve-Path -LiteralPath $ToolRoot).Path
$cargoHome = Join-Path $ToolRoot 'cargo'
$rustupHome = Join-Path $ToolRoot 'rustup'
$mingw = Join-Path $ToolRoot 'w64devkit'
$cargo = Join-Path $cargoHome 'bin\cargo.exe'
$gcc = Join-Path $mingw 'bin\x86_64-w64-mingw32-gcc.exe'
$objdump = Join-Path $mingw 'bin\objdump.exe'
if (!$Fxc) { $Fxc = Join-Path $ToolRoot 'windows-sdk\x64\fxc.exe' }
if (!$GpuiCrate) {
    $GpuiCrate = Get-ChildItem -Path (Join-Path $cargoHome 'registry\cache\*\gpui-0.2.2.crate') -File |
        Select-Object -First 1 -ExpandProperty FullName
}
foreach ($file in @($cargo, $gcc, $objdump, $Fxc, $GpuiCrate)) {
    if (!$file -or !(Test-Path -LiteralPath $file -PathType Leaf)) {
        throw 'Missing Win7 build input. See docs/Win7部署说明.md for tool and dependency setup.'
    }
}
$Fxc = (Resolve-Path -LiteralPath $Fxc).Path
$GpuiCrate = (Resolve-Path -LiteralPath $GpuiCrate).Path
if ((Get-FileHash -LiteralPath $GpuiCrate -Algorithm SHA256).Hash -ne $gpuiSha256) {
    throw 'gpui 0.2.2 archive SHA-256 mismatch.'
}

# Stage the committed source so the default workspace and lockfile still use upstream GPUI.
$source = Join-Path $BuildDirectory ("source-" + [guid]::NewGuid().ToString('N').Substring(0, 12))
[void](New-Item -ItemType Directory -Path $source, $TargetDirectory -Force)
$archive = Join-Path $source 'source.tar'
$buildInputs = @('Cargo.toml', 'Cargo.lock', 'README.md', 'resources', 'scripts',
                 'speaky-core', 'speaky-gpui', 'speaky-win7')
& git -C $root archive --format=tar "--output=$archive" HEAD -- @buildInputs
if ($LASTEXITCODE -ne 0) { throw 'Could not archive the current source commit.' }
& tar.exe -xf $archive -C $source
if ($LASTEXITCODE -ne 0) { throw 'Could not extract the source archive.' }
Remove-Item -LiteralPath $archive
$support = Join-Path $source 'scripts\win7-gpui'
$vendor = Join-Path $source 'vendor'
[void](New-Item -ItemType Directory -Path $vendor)
& tar.exe -xf $GpuiCrate -C $vendor
if ($LASTEXITCODE -ne 0) { throw 'Could not extract the verified GPUI crate.' }
$gpui = Join-Path $vendor 'gpui-0.2.2'
$patch = Join-Path $support 'gpui-0.2.2.patch'
$patchText = [IO.File]::ReadAllText($patch).Replace("`r`n", "`n")
[IO.File]::WriteAllText($patch, $patchText)
$oldCeiling = $env:GIT_CEILING_DIRECTORIES
try {
    $env:GIT_CEILING_DIRECTORIES = $vendor
    & git -C $gpui -c core.autocrlf=false apply --unidiff-zero $patch
    if ($LASTEXITCODE -ne 0) { throw 'Could not apply the Win7 GPUI patch.' }
} finally {
    $env:GIT_CEILING_DIRECTORIES = $oldCeiling
}

$manifest = Join-Path $source 'Cargo.toml'
[IO.File]::AppendAllText($manifest, [Environment]::NewLine + @'
[patch.crates-io]
gpui = { path = "vendor/gpui-0.2.2" }

'@)
$lockfile = Join-Path $source 'Cargo.lock'
$lock = [IO.File]::ReadAllText($lockfile)
$entry = [regex]::new('(?m)(^\[\[package\]\]\r?\nname = "gpui"\r?\nversion = "0\.2\.2"\r?\n)source = "registry\+https://github\.com/rust-lang/crates\.io-index"\r?\nchecksum = "' + $gpuiSha256 + '"\r?\n')
if ($entry.Matches($lock).Count -ne 1) { throw 'Expected the verified gpui 0.2.2 registry entry.' }
[IO.File]::WriteAllText($lockfile, $entry.Replace($lock, '$1', 1))

$bundled = Join-Path $rustupHome "toolchains\$toolchain\lib\rustlib\x86_64-pc-windows-gnu\lib\self-contained"
$settings = @{
    RUSTUP_HOME = $rustupHome
    CARGO_HOME = $cargoHome
    Path = "$cargoHome\bin;$mingw\bin;$env:Path"
    LIBRARY_PATH = "$bundled;$mingw\lib"
    RUSTC_BOOTSTRAP = '1'
    CARGO_TARGET_DIR = $TargetDirectory
    CARGO_TARGET_X86_64_WIN7_WINDOWS_GNU_LINKER = $gcc
    CARGO_TARGET_X86_64_WIN7_WINDOWS_GNU_RUSTFLAGS = '-C target-feature=+crt-static -C link-self-contained=no -C link-arg=-Wl,--major-subsystem-version,6,--minor-subsystem-version,1'
    CARGO_PROFILE_DEV_DEBUG = '0'
    CARGO_PROFILE_DEV_DEBUG_ASSERTIONS = 'false'
    CARGO_PROFILE_DEV_BUILD_OVERRIDE_DEBUG = '0'
    CARGO_PROFILE_DEV_BUILD_OVERRIDE_DEBUG_ASSERTIONS = 'false'
    GPUI_FXC_PATH = $Fxc
}
$previous = @{}
try {
    foreach ($key in $settings.Keys) {
        $previous[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
        [Environment]::SetEnvironmentVariable($key, $settings[$key], 'Process')
    }
    $objects = foreach ($name in @('winrt', 'clock', 'system')) {
        $object = Join-Path $source "$name.o"
        & $gcc -Os -c (Join-Path $support "$name.c") -o $object
        if ($LASTEXITCODE -ne 0) { throw "Could not compile the $name compatibility shim." }
        $object
    }
    $wrapSymbols = @(
        'RoGetActivationFactory', 'RoOriginateErrorW', 'CoIncrementMTAUsage',
        'GetSystemTimePreciseAsFileTime', 'CreateFileMappingFromApp', 'MapViewOfFileFromApp',
        'GetDiskSpaceInformationW', 'GetFirmwareType', 'IsNativeVhdBoot'
    )
    $wrapFlags = foreach ($symbol in $wrapSymbols) {
        "--wrap=$symbol"
        "--wrap=__imp_$symbol"
    }
    $args = @("+$toolchain", '-Z', 'build-std=std,panic_unwind', 'rustc',
              '--manifest-path', $manifest, '--locked', '-p', 'speaky-gpui', '--target', $target)
    if ($Offline) { $args += '--offline' }
    $args += @('--', '-C', ('link-arg=-Wl,' + ($wrapFlags -join ',')))
    foreach ($object in $objects) { $args += @('-C', "link-arg=$object") }
    $args += @('-C', 'link-arg=-lkernel32')
    & $cargo @args
    if ($LASTEXITCODE -ne 0) { throw 'Win7 GPUI build failed.' }

    $exe = Join-Path $TargetDirectory "$target\debug\speaky-gpui.exe"
    $badDlls = @(
        'api-ms-win-core-winrt-error-l1-1-0.dll', 'api-ms-win-core-winrt-l1-1-0.dll',
        'api-ms-win-core-synch-l1-2-0.dll', 'api-ms-win-shcore-scaling-l1-1-1.dll',
        'bcryptprimitives.dll', 'dcomp.dll', 'd3dcompiler_47.dll', 'icuuc.dll', 'icuin.dll', 'icu.dll'
    )
    $badSymbols = $wrapSymbols + @(
        'CreateDXGIFactory2', 'DXGIGetDebugInterface1', 'DCompositionCreateDevice', 'D3DCompileFromFile',
        'GetDpiForWindow', 'GetDpiForMonitor', 'GetSystemMetricsForDpi', 'GetPointerInfo', 'GetPointerType',
        'GetPointerPenInfo', 'GetPointerTouchInfo', 'AdjustWindowRectExForDpi',
        'SetThreadDpiAwarenessContext', 'SetProcessDpiAwarenessContext',
        'WaitOnAddress', 'WakeByAddressAll', 'WakeByAddressSingle', 'ProcessPrng'
    )
    $dllPattern = ($badDlls | ForEach-Object { [regex]::Escape($_) }) -join '|'
    $symbolPattern = ($badSymbols | ForEach-Object { [regex]::Escape($_) }) -join '|'
    $badPattern = "DLL Name:\s*(?:$dllPattern)\s*$|\b(?:$symbolPattern)\s*$"
    $imports = @(& $objdump -p $exe | Select-String -Pattern '^\s*(?:\w*SubsystemVersion|Subsystem|DLL Name:)', $badPattern)
    if ($LASTEXITCODE -ne 0) { throw 'Could not inspect the Win7 executable imports.' }
    $bad = @($imports | Where-Object { $_.Line -match $badPattern })
    if ($bad.Count) { throw "Win7-incompatible PE imports: $($bad.Line -join ', ')" }
    if (!($imports.Line -match 'MajorSubsystemVersion\s+6\s*$') -or
        !($imports.Line -match 'MinorSubsystemVersion\s+1\s*$') -or
        !($imports.Line -match 'Subsystem\s+0*2\s+\(Windows GUI\)')) {
        throw 'Expected the Windows GUI subsystem version 6.01.'
    }
    $imports.Line | Set-Content -LiteralPath (Join-Path $BuildDirectory 'imports.txt') -Encoding ascii
    Get-FileHash -LiteralPath $exe -Algorithm SHA256
} finally {
    foreach ($key in $previous.Keys) {
        if ($null -eq $previous[$key]) {
            Remove-Item -LiteralPath "Env:$key" -ErrorAction SilentlyContinue
        } else {
            [Environment]::SetEnvironmentVariable($key, $previous[$key], 'Process')
        }
    }
}
