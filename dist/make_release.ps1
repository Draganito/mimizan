# Builds the Windows release archive for mimizan.
#
# Result (always under dist/, not committed):
#   dist/mimizan-<version>-x86_64-windows.zip
#     mimizan.exe, mimizan-gui.exe, cameras/, look/, README, manual, license
#   dist/mimizan-<version>-x64.msixupload
#     the Store package (make_msix.ps1)
# The programs carry the Visual C++ runtime. mimizan-gui.exe opens no console.
#
# Usage:  dist/make_release.ps1 [-SkipBuild]

param(
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

$ProjectDir = Split-Path -Parent $PSScriptRoot
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $ProjectDir "target" }
$Cli = Join-Path $TargetDir "dist\mimizan.exe"
$Gui = Join-Path $TargetDir "dist\mimizan-gui.exe"

if (-not $SkipBuild) {
    Write-Host "==> cargo build --profile dist"
    Push-Location $ProjectDir
    try {
        cargo build --profile dist -p mimizan-cli -p mimizan-gui
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    } finally {
        Pop-Location
    }
}

foreach ($binary in @($Cli, $Gui)) {
    if (-not (Test-Path -LiteralPath $binary)) {
        throw "ERROR: $binary missing"
    }
    Write-Host "==> binary: $binary"
}
& "$PSScriptRoot\check_windows_exe.ps1" -Path $Cli -Kind console
& "$PSScriptRoot\check_windows_exe.ps1" -Path $Gui -Kind gui

$versionLine = (& $Cli --version | Out-String).Trim()
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$Version = ($versionLine -split "\s+", 2)[1]
if (-not $Version) { throw "ERROR: could not read version from: $versionLine" }
Write-Host "==> version $Version"

& $Cli measure synth --scene wedge --size 256 | Out-Null
if ($LASTEXITCODE -ne 0) { throw "ERROR: measure synth failed" }

$Name = "mimizan-$Version-x86_64-windows"
$Stage = Join-Path ([System.IO.Path]::GetTempPath()) ("mimizan-" + [guid]::NewGuid().ToString("n"))
$Root = Join-Path $Stage $Name
New-Item -ItemType Directory -Path $Root -Force | Out-Null
Copy-Item -LiteralPath $Cli, $Gui -Destination $Root
Copy-Item -LiteralPath (Join-Path $ProjectDir "cameras") -Destination $Root -Recurse
Copy-Item -LiteralPath (Join-Path $ProjectDir "look") -Destination $Root -Recurse
foreach ($doc in @("README.md", "LICENSE")) {
    Copy-Item -LiteralPath (Join-Path $ProjectDir $doc) -Destination $Root
}

$Dist = Join-Path $ProjectDir "dist"
New-Item -ItemType Directory -Path $Dist -Force | Out-Null
Get-ChildItem -LiteralPath $Dist -Filter "mimizan-*-x86_64-windows.zip" -ErrorAction SilentlyContinue |
    Remove-Item -Force
$Zip = Join-Path $Dist "$Name.zip"
tar.exe -C $Stage -a -c -f $Zip $Name
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Remove-Item -LiteralPath $Stage -Recurse -Force
Write-Host "==> $Zip"
Get-Item -LiteralPath $Zip | Select-Object FullName, Length

& pwsh -NoProfile -STA -ExecutionPolicy Bypass -File "$PSScriptRoot\make_msix.ps1" -SkipBuild
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
