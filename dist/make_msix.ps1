# Builds the Microsoft Store package for mimizan.
#
# Result (always under dist/, not committed):
#   dist/mimizan-<version>-x64.msix
#   dist/mimizan-<version>-x64.msixupload
# The upload file is what Partner Center takes. The Store signs it again.
# The certificate made here only lets this machine install the package for the check.
# mimizan.exe sits next to the window. It is not its own app: an individual
# Store account cannot submit an app without a window.
#
# Usage:  pwsh -STA -File dist/make_msix.ps1 [-SkipBuild]

param(
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

$ProjectDir = Split-Path -Parent $PSScriptRoot
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $ProjectDir "target" }
$Cli = Join-Path $TargetDir "dist\mimizan.exe"
$Gui = Join-Path $TargetDir "dist\mimizan-gui.exe"
$Publisher = "CN=0C88F7B4-325D-4257-BFCA-6F64B313E1BC"
$IdentityName = "DraganBojovic.MimizanLab"

if (-not $IsWindows) { throw "ERROR: make_msix.ps1 runs on Windows" }
if ([System.Threading.Thread]::CurrentThread.GetApartmentState() -ne [System.Threading.ApartmentState]::STA) {
    throw "ERROR: make_msix.ps1 needs pwsh -STA"
}

function Find-SdkTool([string]$Name) {
    $root = "${env:ProgramFiles(x86)}\Windows Kits\10\bin"
    if (-not (Test-Path -LiteralPath $root)) { throw "ERROR: Windows SDK not found at $root" }
    $kits = Get-ChildItem -LiteralPath $root -Directory | Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' }
    foreach ($kit in ($kits | Sort-Object { [version]$_.Name } -Descending)) {
        $path = Join-Path $kit.FullName "x64\$Name"
        if (Test-Path -LiteralPath $path) { return $path }
    }
    throw "ERROR: $Name not found under $root"
}

function Save-Tile([string]$Source, [string]$Dest, [int]$Width, [int]$Height) {
    $src = New-Object System.Windows.Media.Imaging.BitmapImage
    $src.BeginInit()
    $src.UriSource = [System.Uri]::new((Resolve-Path -LiteralPath $Source).ProviderPath)
    $src.CacheOption = [System.Windows.Media.Imaging.BitmapCacheOption]::OnLoad
    $src.EndInit()
    $src.Freeze()

    $visual = New-Object System.Windows.Media.DrawingVisual
    [System.Windows.Media.RenderOptions]::SetBitmapScalingMode(
        $visual, [System.Windows.Media.BitmapScalingMode]::HighQuality)
    $dc = $visual.RenderOpen()
    $dc.DrawRectangle(
        [System.Windows.Media.Brushes]::Black, $null,
        (New-Object System.Windows.Rect 0, 0, $Width, $Height))
    $scale = [Math]::Min($Width / $src.PixelWidth, $Height / $src.PixelHeight)
    $dw = $src.PixelWidth * $scale
    $dh = $src.PixelHeight * $scale
    $dc.DrawImage($src, (New-Object System.Windows.Rect (($Width - $dw) / 2), (($Height - $dh) / 2), $dw, $dh))
    $dc.Close()

    $rtb = New-Object System.Windows.Media.Imaging.RenderTargetBitmap(
        $Width, $Height, 96, 96, [System.Windows.Media.PixelFormats]::Pbgra32)
    $rtb.Render($visual)
    $flat = New-Object System.Windows.Media.Imaging.FormatConvertedBitmap
    $flat.BeginInit()
    $flat.Source = $rtb
    $flat.DestinationFormat = [System.Windows.Media.PixelFormats]::Bgr24
    $flat.EndInit()
    if ($flat.CanFreeze) { $flat.Freeze() }
    $encoder = New-Object System.Windows.Media.Imaging.PngBitmapEncoder
    $encoder.Frames.Add([System.Windows.Media.Imaging.BitmapFrame]::Create($flat))
    $stream = [System.IO.File]::Open($Dest, [System.IO.FileMode]::Create)
    try { $encoder.Save($stream) } finally { $stream.Close() }

    $bytes = [System.IO.File]::ReadAllBytes($Dest)
    if ($bytes.Length -lt 26 -or $bytes[0] -ne 0x89) { throw "ERROR: $Dest is not a PNG" }
    $gotW = ([int]$bytes[16] * 16777216) + ([int]$bytes[17] * 65536) + ([int]$bytes[18] * 256) + [int]$bytes[19]
    $gotH = ([int]$bytes[20] * 16777216) + ([int]$bytes[21] * 65536) + ([int]$bytes[22] * 256) + [int]$bytes[23]
    if ($gotW -ne $Width -or $gotH -ne $Height) {
        throw "ERROR: $Dest is ${gotW}x${gotH}, expected ${Width}x${Height}"
    }
    # PNG color type 2 is RGB. Anything else still has a transparent channel or a palette.
    if ($bytes[25] -ne 2) { throw "ERROR: $Dest color type is $($bytes[25]), expected opaque RGB" }
}

if (-not $SkipBuild) {
    Write-Host "==> cargo build --profile dist"
    Push-Location $ProjectDir
    try {
        cargo build --profile dist -p mimizan-cli -p mimizan-gui
        if ($LASTEXITCODE -ne 0) { throw "ERROR: cargo build failed" }
    } finally {
        Pop-Location
    }
}

foreach ($binary in @($Cli, $Gui)) {
    if (-not (Test-Path -LiteralPath $binary)) { throw "ERROR: $binary missing" }
    Write-Host "==> binary: $binary"
}
& "$PSScriptRoot\check_windows_exe.ps1" -Path $Cli -Kind console
& "$PSScriptRoot\check_windows_exe.ps1" -Path $Gui -Kind gui

$versionLine = (& $Cli --version | Out-String).Trim()
if ($LASTEXITCODE -ne 0) { throw "ERROR: mimizan --version failed" }
$Version = ($versionLine -split "\s+", 2)[1]
if ($Version -notmatch '^\d+\.\d+\.\d+$') {
    throw "ERROR: MSIX version needs major.minor.patch, got: $Version"
}
$PackageVersion = "$Version.0"
Write-Host "==> version $PackageVersion"

& $Cli measure synth --scene wedge --size 256 | Out-Null
if ($LASTEXITCODE -ne 0) { throw "ERROR: measure synth failed" }

$MakeAppx = Find-SdkTool "makeappx.exe"
$SignTool = Find-SdkTool "signtool.exe"
Write-Host "==> $MakeAppx"

Add-Type -AssemblyName PresentationCore

$Stage = Join-Path ([System.IO.Path]::GetTempPath()) ("mimizan-msix-" + [guid]::NewGuid().ToString("n"))
$Layout = Join-Path $Stage "layout"
$Assets = Join-Path $Layout "Assets"
New-Item -ItemType Directory -Path $Assets -Force | Out-Null
$Thumb = $null
$PackageFullName = $null

try {
    Copy-Item -LiteralPath $Cli, $Gui -Destination $Layout
    Copy-Item -LiteralPath (Join-Path $ProjectDir "cameras") -Destination $Layout -Recurse
    Copy-Item -LiteralPath (Join-Path $ProjectDir "look") -Destination $Layout -Recurse
    foreach ($rel in @("cameras\nikon_z_f.json", "look\neutral.json")) {
        if (-not (Test-Path -LiteralPath (Join-Path $Layout $rel))) {
            throw "ERROR: layout is missing $rel"
        }
    }

    $Icon = Join-Path $ProjectDir "dist\icons\mimizan-256.png"
    if (-not (Test-Path -LiteralPath $Icon)) { throw "ERROR: $Icon missing" }
    Save-Tile $Icon (Join-Path $Assets "Square44x44Logo.png") 44 44
    Save-Tile $Icon (Join-Path $Assets "StoreLogo.png") 50 50
    Save-Tile $Icon (Join-Path $Assets "Square150x150Logo.png") 150 150
    Save-Tile $Icon (Join-Path $Assets "Wide310x150Logo.png") 310 150

    $manifest = @"
<?xml version="1.0" encoding="utf-8"?>
<Package
  xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
  xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10"
  xmlns:rescap="http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"
  IgnorableNamespaces="rescap">
  <Identity
    Name="$IdentityName"
    Publisher="$Publisher"
    Version="$PackageVersion"
    ProcessorArchitecture="x64" />
  <Properties>
    <DisplayName>Mimizan Lab</DisplayName>
    <PublisherDisplayName>Dragan Bojovic</PublisherDisplayName>
    <Logo>Assets\StoreLogo.png</Logo>
  </Properties>
  <Dependencies>
    <TargetDeviceFamily Name="Windows.Desktop" MinVersion="10.0.17763.0" MaxVersionTested="10.0.26100.0" />
  </Dependencies>
  <Resources>
    <Resource Language="en-us" />
  </Resources>
  <Applications>
    <Application Id="Lab" Executable="mimizan-gui.exe" EntryPoint="Windows.FullTrustApplication">
      <uap:VisualElements
        DisplayName="Mimizan Lab"
        Description="A darkroom for the digital negative."
        BackgroundColor="#000000"
        Square150x150Logo="Assets\Square150x150Logo.png"
        Square44x44Logo="Assets\Square44x44Logo.png">
        <uap:DefaultTile Wide310x150Logo="Assets\Wide310x150Logo.png" />
      </uap:VisualElements>
    </Application>
  </Applications>
  <Capabilities>
    <rescap:Capability Name="runFullTrust" />
  </Capabilities>
</Package>
"@
    $utf8 = New-Object System.Text.UTF8Encoding $false
    [System.IO.File]::WriteAllText((Join-Path $Layout "AppxManifest.xml"), $manifest.TrimStart(), $utf8)

    $Dist = Join-Path $ProjectDir "dist"
    New-Item -ItemType Directory -Path $Dist -Force | Out-Null
    Get-ChildItem -LiteralPath $Dist -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like "mimizan-*-x64.msix" -or $_.Name -like "mimizan-*-x64.msixupload" } |
        Remove-Item -Force
    $Msix = Join-Path $Dist "mimizan-$Version-x64.msix"
    $Upload = Join-Path $Dist "mimizan-$Version-x64.msixupload"

    & $MakeAppx pack /d $Layout /p $Msix /h SHA256 /o
    if ($LASTEXITCODE -ne 0) { throw "ERROR: makeappx failed" }

    $cert = New-SelfSignedCertificate `
        -Type Custom `
        -Subject $Publisher `
        -CertStoreLocation "Cert:\CurrentUser\My" `
        -KeyAlgorithm RSA `
        -KeyLength 2048 `
        -KeyExportPolicy Exportable `
        -KeyUsage DigitalSignature `
        -NotAfter (Get-Date).AddYears(1) `
        -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3")
    $Thumb = $cert.Thumbprint
    if ($cert.Subject -ne $Publisher) {
        throw "ERROR: certificate subject is $($cert.Subject), expected $Publisher"
    }
    $pfxPassword = [guid]::NewGuid().ToString("n")
    $secure = ConvertTo-SecureString -String $pfxPassword -Force -AsPlainText
    $pfx = Join-Path $Stage "mimizan.pfx"
    $cer = Join-Path $Stage "mimizan.cer"
    Export-PfxCertificate -Cert $cert -FilePath $pfx -Password $secure | Out-Null
    Export-Certificate -Cert $cert -FilePath $cer | Out-Null

    Write-Host "==> sign"
    & $SignTool sign /fd SHA256 /f $pfx /p $pfxPassword $Msix
    if ($LASTEXITCODE -ne 0) { throw "ERROR: signtool failed" }

    $unlock = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock"
    if (-not (Test-Path -LiteralPath $unlock)) { New-Item -Path $unlock -Force | Out-Null }
    New-ItemProperty -Path $unlock -Name AllowAllTrustedApps -Value 1 -PropertyType DWord -Force | Out-Null
    Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\TrustedPeople | Out-Null

    Get-AppxPackage -Name $IdentityName | Remove-AppxPackage -ErrorAction SilentlyContinue
    Add-AppxPackage -Path $Msix
    $installed = Get-AppxPackage -Name $IdentityName
    if (-not $installed) { throw "ERROR: $IdentityName did not install" }
    $PackageFullName = $installed.PackageFullName
    foreach ($rel in @("mimizan-gui.exe", "mimizan.exe", "cameras\nikon_z_f.json", "look\neutral.json")) {
        if (-not (Test-Path -LiteralPath (Join-Path $installed.InstallLocation $rel))) {
            throw "ERROR: installed package is missing $rel"
        }
    }
    Write-Host "==> installed $($installed.PackageFullName)"

    # -a picks the format from the extension. .msixupload is not a name it
    # knows, so the archive has to be born as a .zip and renamed after.
    $UploadDir = Join-Path $Stage "upload"
    $UploadZip = Join-Path $Stage "upload.zip"
    New-Item -ItemType Directory -Path $UploadDir -Force | Out-Null
    Copy-Item -LiteralPath $Msix -Destination $UploadDir
    tar.exe -C $UploadDir -a -c -f $UploadZip (Split-Path -Leaf $Msix)
    if ($LASTEXITCODE -ne 0) { throw "ERROR: msixupload failed" }
    Move-Item -LiteralPath $UploadZip -Destination $Upload -Force
    $listed = @(& tar.exe -tf $Upload | ForEach-Object { $_.Trim() })
    if ($LASTEXITCODE -ne 0) { throw "ERROR: could not list msixupload" }
    if ($listed -notcontains (Split-Path -Leaf $Msix)) {
        throw "ERROR: msixupload does not contain $(Split-Path -Leaf $Msix)"
    }
    $magic = [System.IO.File]::ReadAllBytes($Upload)[0..1]
    if ($magic[0] -ne 0x50 -or $magic[1] -ne 0x4B) { throw "ERROR: msixupload is not a zip" }
    Write-Host "==> $Upload"
    Get-Item -LiteralPath $Msix, $Upload | Select-Object FullName, Length
} finally {
    try {
        if ($PackageFullName) {
            Remove-AppxPackage -Package $PackageFullName -ErrorAction SilentlyContinue
        }
        if ($Thumb) {
            Get-ChildItem Cert:\LocalMachine\TrustedPeople, Cert:\CurrentUser\My -ErrorAction SilentlyContinue |
                Where-Object { $_.Thumbprint -eq $Thumb } |
                Remove-Item -Confirm:$false -ErrorAction SilentlyContinue
        }
        if (Test-Path -LiteralPath $Stage) {
            Remove-Item -LiteralPath $Stage -Recurse -Force -ErrorAction SilentlyContinue
        }
    } catch {
        Write-Host "==> cleanup: $($_.Exception.Message)"
    }
}
