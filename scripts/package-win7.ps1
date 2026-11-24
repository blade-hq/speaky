param(
    [string]$Executable = 'target/x86_64-win7-windows-msvc/release/speaky-win7.exe',
    [string]$Output = 'Speaky-Windows7-x64.zip'
)
$ErrorActionPreference = 'Stop'
$exe = Get-Item -LiteralPath $Executable
$bytes = [IO.File]::ReadAllBytes($exe.FullName)

function Read-U16([int]$offset) { [BitConverter]::ToUInt16($bytes, $offset) }
function Read-U32([int]$offset) { [BitConverter]::ToUInt32($bytes, $offset) }
function Read-AsciiZ([int]$offset) {
    $end = [Array]::IndexOf($bytes, [byte]0, $offset)
    if ($end -le $offset) { throw "Invalid PE import name at offset $offset." }
    [Text.Encoding]::ASCII.GetString($bytes, $offset, $end - $offset)
}

if ((Read-U16 0) -ne 0x5a4d) { throw 'Expected an MZ executable.' }
$pe = Read-U32 0x3c
$opt = $pe + 24
if ((Read-U32 $pe) -ne 0x4550 -or (Read-U16 ($pe + 4)) -ne 0x8664 -or (Read-U16 $opt) -ne 0x20b) {
    throw 'Expected an x64 PE32+ executable.'
}
if ((Read-U16 ($opt + 48)) -ne 6 -or (Read-U16 ($opt + 50)) -ne 1 -or (Read-U16 ($opt + 68)) -ne 2) {
    throw 'Expected Windows GUI subsystem version 6.01.'
}

function Convert-Rva([uint32]$rva) {
    for ($i = 0; $i -lt (Read-U16 ($pe + 6)); $i++) {
        $section = $opt + (Read-U16 ($pe + 20)) + 40 * $i
        $start = Read-U32 ($section + 12)
        $length = Read-U32 ($section + 16)
        if ($rva -ge $start -and ($rva - $start) -lt $length) {
            $offset = [long](Read-U32 ($section + 20)) + $rva - $start
            if ($offset -ge $bytes.Length) { break }
            return [int]$offset
        }
    }
    throw "Invalid PE import RVA $rva."
}

function Read-Imports {
    $directoryCount = Read-U32 ($opt + 108)
    foreach ($index in 1, 13) {
        if ($index -ge $directoryCount) { continue }
        $directory = $opt + 112 + 8 * $index
        $rva = Read-U32 $directory
        if ($rva -eq 0) { continue }
        $offset = Convert-Rva $rva
        $size = Read-U32 ($directory + 4)
        $stride = if ($index -eq 1) { 20 } else { 32 }
        $terminated = $false
        for ($at = $offset; ($at - $offset + $stride) -le $size; $at += $stride) {
            if ($index -eq 1) {
                $name = Read-U32 ($at + 12)
                $thunks = Read-U32 $at
                if ($thunks -eq 0) { $thunks = Read-U32 ($at + 16) }
            } else {
                $name = Read-U32 ($at + 4)
                $thunks = Read-U32 ($at + 16)
                if ($name -ne 0 -and (Read-U32 $at) -ne 1) {
                    throw 'Unsupported VA-based delay import directory.'
                }
            }
            if ($name -eq 0) { $terminated = $true; break }
            Read-AsciiZ (Convert-Rva $name)
            $entry = Convert-Rva $thunks
            while ($true) {
                $low = Read-U32 $entry
                $high = Read-U32 ($entry + 4)
                if ($low -eq 0 -and $high -eq 0) { break }
                if ($high -ne 0) {
                    throw 'Unexpected ordinal-only or invalid import in the Win7 executable.'
                }
                Read-AsciiZ ((Convert-Rva $low) + 2)
                $entry += 8
            }
        }
        if (!$terminated) { throw 'Unterminated PE import directory.' }
    }
}

$dumpbin = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
if ($dumpbin) {
    $dump = & $dumpbin.Source /nologo /imports $exe.FullName
    if ($LASTEXITCODE -ne 0) { throw 'dumpbin failed while reading imports.' }
    # DUMPBIN output names each library and lists its named imports on indented rows.
    $imports = foreach ($line in $dump) {
        if ($line -match '^\s*([\w.-]+\.dll)\s*$' -or $line -match '^\s+[0-9A-F]+\s+([\w@?]+)\s*$') {
            $Matches[1]
        } elseif ($line -match '^\s*(?:Ordinal\s+\d+|\d+\s+Ordinal\s+\d+)\s*$') {
            throw 'Unexpected ordinal-only import in the Win7 executable.'
        }
    }
    # Check the binary as well so output formatting cannot hide delay-load imports.
    $imports = @($imports) + @(Read-Imports)
} else {
    $imports = @(Read-Imports)
}
if (!($imports -match '\.dll$')) { throw 'Could not read a PE import library.' }

$blockedDll = '^(?:shcore|dcomp|bcryptprimitives|ucrtbase|vcruntime\w*|msvcp\d\w*|msvcr\d\w*)\.dll$|^(?:api-ms-win-shcore-|api-ms-win-crt-|ext-ms-)|^api-ms-win-core-synch-l1-2-0\.dll$'
$blockedFunction = '^(?:GetDpiForMonitor|GetDpiForWindow|GetSystemDpiForProcess|GetDpiForSystem|AdjustWindowRectExForDpi|EnableNonClientDpiScaling|SetProcessDpiAwareness|SetProcessDpiAwarenessContext|SetThreadDpiAwarenessContext|ProcessPrng|WaitOnAddress|WakeByAddressSingle|WakeByAddressAll|GetSystemTimePreciseAsFileTime|CreateFile2|SetThreadDescription|GetThreadDescription|GetCurrentThreadStackLimits|GetSystemCpuSetInformation|GetOverlappedResultEx|GetTempPath2[AW]|GetPackageFullName|GetCurrentPackageId|RoInitialize|RoGetActivationFactory)$'
$blocked = $imports | Where-Object { $_ -match $blockedDll -or $_ -match $blockedFunction } | Sort-Object -Unique
if ($blocked) { throw "Unsupported Win7 or dynamic CRT imports: $($blocked -join ', ')." }

Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$root = Split-Path $PSScriptRoot -Parent
$files = [ordered]@{
    'speaky-win7.exe' = $exe.FullName
    'config.example.yaml' = Join-Path $root 'config.example.yaml'
    'docs/Win7部署说明.md' = Join-Path $root 'docs/Win7部署说明.md'
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
