# Builds the Windows release archive for mimizan.
#
# Result (always under dist/, not committed):
#   dist/mimizan-<version>-x86_64-windows.zip
#     mimizan.exe, mimizan-gui.exe, cameras/, look/, docs
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
New-Item -ItemType Directory -Path (Join-Path $Root "docs") -Force | Out-Null
Copy-Item -LiteralPath $Cli, $Gui -Destination $Root
Copy-Item -LiteralPath (Join-Path $ProjectDir "cameras") -Destination $Root -Recurse
Copy-Item -LiteralPath (Join-Path $ProjectDir "look") -Destination $Root -Recurse
foreach ($doc in @("README.md", "DETAILED.md", "LICENSE")) {
    Copy-Item -LiteralPath (Join-Path $ProjectDir $doc) -Destination $Root
}
Copy-Item -LiteralPath (Join-Path $ProjectDir "third_party\librtprocess\LICENSE.txt") -Destination (Join-Path $Root "LICENSE-librtprocess.txt")
foreach ($doc in @("SPEC.md", "RESULTS.md", "gui.png")) {
    Copy-Item -LiteralPath (Join-Path $ProjectDir "docs\$doc") -Destination (Join-Path $Root "docs")
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
