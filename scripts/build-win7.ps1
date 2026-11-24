$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$temp = Join-Path ([IO.Path]::GetTempPath()) "speaky-win7-$([guid]::NewGuid())"
$oldFlags = $env:CARGO_ENCODED_RUSTFLAGS
New-Item -ItemType Directory -Path $temp | Out-Null
Push-Location $root
try {
    $zip = Join-Path $temp 'YY-Thunks-Objs.zip'
    Invoke-WebRequest 'https://github.com/Chuyu-Team/YY-Thunks/releases/download/v1.1.7/YY-Thunks-Objs.zip' -OutFile $zip
    if ((Get-FileHash -Algorithm SHA256 $zip).Hash -ne 'C3066F3F074EBC2A89B2DEF5F427BDEA238DEE17349D2BAB579AF519781691AB') {
        throw 'YY-Thunks archive SHA256 mismatch.'
    }
    Expand-Archive -LiteralPath $zip -DestinationPath $temp
    $thunks = (Get-Item -LiteralPath (Join-Path $temp 'objs/x64/YY_Thunks_for_Win7.obj')).FullName
    $env:CARGO_ENCODED_RUSTFLAGS = @('-C', 'target-feature=+crt-static', '-C', "link-arg=$thunks", '-C', 'link-arg=/SUBSYSTEM:WINDOWS,6.01') -join [char]31
    cargo +nightly build --release -p speaky-win7 --target x86_64-win7-windows-msvc -Z build-std=std,panic_unwind --locked
    if ($LASTEXITCODE -ne 0) { throw "Win7 build failed ($LASTEXITCODE)." }
    ./scripts/package-win7.ps1
} finally {
    Pop-Location
    $env:CARGO_ENCODED_RUSTFLAGS = $oldFlags
    Remove-Item -LiteralPath $temp -Recurse -Force
}
