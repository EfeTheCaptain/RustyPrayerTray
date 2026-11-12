<#
.SYNOPSIS
    Build a fully standalone, portable RustyPrayerTray.exe (static CRT; no VC++ runtime needed).

.DESCRIPTION
    Performs a release build for x86_64-pc-windows-msvc with +crt-static (see
    rust/.cargo/config.toml), then stages a portable folder and a zip under dist\.
    The resulting exe runs on a clean Windows 10/11 x64 machine with no installer and
    no Microsoft Visual C++ Redistributable.

.PARAMETER Configuration
    Cargo profile to build: release (default) or debug.

.PARAMETER SkipBuild
    Reuse an existing build in rust\target instead of invoking cargo.

.PARAMETER NoPortableData
    Do not write the `portable` marker. Without it, settings live in
    %APPDATA%\PrayerTray instead of beside the exe.

.PARAMETER OutDir
    Output directory. Default: <repo>\dist.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\make-portable.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\make-portable.ps1 -Configuration release -NoPortableData
#>
[CmdletBinding()]
param(
    [ValidateSet('release', 'debug')]
    [string]$Configuration = 'release',
    [switch]$SkipBuild,
    [switch]$NoPortableData,
    [string]$OutDir
)

$ErrorActionPreference = 'Stop'

$RepoRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$RustDir  = Join-Path $RepoRoot 'rust'
$Target   = 'x86_64-pc-windows-msvc'
$ExeName  = 'RustyPrayerTray.exe'
$DistDir  = if ($OutDir) { [System.IO.Path]::GetFullPath($OutDir) } else { Join-Path $RepoRoot 'dist' }
$StageDir = Join-Path $DistDir 'PrayerTray-portable'
$ZipPath  = Join-Path $DistDir 'PrayerTray-portable-win-x64.zip'

function Find-VcVars {
    # Already in a VS Developer shell? Then the environment is set up.
    if ($env:VSCMD_ARG_TGT_ARCH) { return $null }

    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path -LiteralPath $vswhere) {
        $install = & $vswhere -latest -products * `
            -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
            -property installationPath 2>$null
        if ($install) {
            $cand = Join-Path $install.Trim() 'VC\Auxiliary\Build\vcvars64.bat'
            if (Test-Path -LiteralPath $cand) { return $cand }
        }
    }

    $probe = @(
        'C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat',
        'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat',
        'C:\Program Files\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat',
        'C:\Program Files\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
    )
    foreach ($p in $probe) { if (Test-Path -LiteralPath $p) { return $p } }
    return $null
}

$cargoArgs = @('build')
if ($Configuration -eq 'release') { $cargoArgs += '--release' }
$cargoArgs += @('--target', $Target, '--locked')
$cargoCmd = $cargoArgs -join ' '

if (-not $SkipBuild) {
    Push-Location $RustDir
    try {
        $vcvars = Find-VcVars
        if ($vcvars) {
            Write-Host "Building (static CRT): call `"$vcvars`" && cargo $cargoCmd"
            & cmd.exe /c "call `"$vcvars`" && cargo $cargoCmd"
        } else {
            Write-Host "Building with the current shell environment: cargo $cargoCmd"
            & cargo @cargoArgs
        }
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit code $LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
}

$profileDir = if ($Configuration -eq 'release') { 'release' } else { 'debug' }
$builtExe   = Join-Path $RustDir "target\$Target\$profileDir\$ExeName"
if (-not (Test-Path -LiteralPath $builtExe)) {
    throw "Build output not found: $builtExe (run without -SkipBuild first)"
}

if (Test-Path -LiteralPath $StageDir) { Remove-Item -LiteralPath $StageDir -Recurse -Force }
New-Item -ItemType Directory -Path $StageDir -Force | Out-Null

$destExe = Join-Path $StageDir 'RustyPrayerTray.exe'
Copy-Item -LiteralPath $builtExe -Destination $destExe -Force

$bareExe = Join-Path $DistDir 'PrayerTray-win-x64.exe'
Copy-Item -LiteralPath $builtExe -Destination $bareExe -Force

if (-not $NoPortableData) {
    New-Item -ItemType File -Path (Join-Path $StageDir 'portable') -Force | Out-Null
}

$dataLine = if ($NoPortableData) {
    'Settings are stored in %APPDATA%\PrayerTray.'
} else {
    'Settings and state are stored in this folder (portable mode).'
}
$readme = @(
    'RustyPrayerTray (portable)',
    '',
    '- Double-click RustyPrayerTray.exe. No installer, no .NET, no VC++ runtime required.',
    "- $dataLine",
    '- Delete this folder to uninstall.',
    '- Windows 10/11 x64.',
    ''
) -join "`r`n"
Set-Content -LiteralPath (Join-Path $StageDir 'README.txt') -Value $readme -Encoding UTF8

if (Test-Path -LiteralPath $ZipPath) { Remove-Item -LiteralPath $ZipPath -Force }
Compress-Archive -Path (Join-Path $StageDir '*') -DestinationPath $ZipPath -CompressionLevel Optimal -Force

$exeMB = [math]::Round((Get-Item -LiteralPath $destExe).Length / 1MB, 2)

Write-Host ''
Write-Host 'Portable build complete' -ForegroundColor Green
Write-Host "  exe    : $destExe ($exeMB MB)"
Write-Host "  bare   : $bareExe"
Write-Host "  folder : $StageDir"
Write-Host "  zip    : $ZipPath"
