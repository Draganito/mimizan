# Fail unless a Windows program has the expected subsystem and does not
# import the Visual C++ redistributable (vcruntime140.dll, vcruntime140_1.dll,
# msvcp140.dll and their kin). The Universal CRT that ships with Windows 10
# is allowed.
param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][ValidateSet("gui", "console")][string]$Kind
)

$ErrorActionPreference = "Stop"
if (-not (Test-Path -LiteralPath $Path)) { throw "ERROR: $Path missing" }

$bytes = [System.IO.File]::ReadAllBytes($Path)
function U16([int]$at) { [BitConverter]::ToUInt16($bytes, $at) }
function U32([int]$at) { [BitConverter]::ToUInt32($bytes, $at) }

$pe = U32 0x3C
if ($pe -le 0 -or ($pe + 256) -ge $bytes.Length) { throw "ERROR: $Path has no PE header" }
$sections = U16 ($pe + 6)
$optSize = U16 ($pe + 20)
$opt = $pe + 24
$magic = U16 $opt
$subsystem = U16 ($opt + 68)
if ($magic -eq 0x20b) {
    $importSlot = $opt + 112 + 8
    $delaySlot = $opt + 112 + (13 * 8)
} elseif ($magic -eq 0x10b) {
    $importSlot = $opt + 96 + 8
    $delaySlot = $opt + 96 + (13 * 8)
} else {
    throw "ERROR: $Path has an unknown optional-header magic $magic"
}

$secStart = $opt + $optSize
function Convert-Rva([uint32]$rva) {
    if ($rva -eq 0) { return -1 }
    for ($i = 0; $i -lt $sections; $i++) {
        $s = $secStart + ($i * 40)
        $va = U32 ($s + 12)
        $size = U32 ($s + 8)
        $rawSize = U32 ($s + 16)
        $span = [Math]::Max($size, $rawSize)
        $raw = U32 ($s + 20)
        if ($rva -ge $va -and $rva -lt ($va + $span)) {
            return [int]($raw + ($rva - $va))
        }
    }
    throw "ERROR: RVA $rva of $Path is not in a section"
}
function Read-CString([int]$at) {
    $end = $at
    while ($bytes[$end] -ne 0) { $end++ }
    [System.Text.Encoding]::ASCII.GetString($bytes, $at, $end - $at)
}
function Read-ImportNames([uint32]$dirRva, [int]$nameAt, [int]$step) {
    $names = @()
    if ($dirRva -eq 0) { return $names }
    $off = Convert-Rva $dirRva
    for ($n = 0; $n -lt 64; $n++) {
        $nameRva = U32 ($off + $nameAt)
        if ($nameRva -eq 0) { break }
        $names += Read-CString (Convert-Rva $nameRva)
        $off += $step
    }
    return $names
}

$imports = @()
$imports += Read-ImportNames (U32 $importSlot) 12 20
$imports += Read-ImportNames (U32 $delaySlot) 4 32

$want = if ($Kind -eq "gui") { 2 } else { 3 }
$label = if ($Kind -eq "gui") { "Windows GUI" } else { "console" }
if ($subsystem -ne $want) {
    throw "ERROR: $Path subsystem is $subsystem, expected $want ($label)"
}
$banned = $imports | Where-Object { $_ -match '^(vcruntime|msvcp|concrt|vcomp)' }
if ($banned) {
    throw "ERROR: $Path imports the Visual C++ redistributable: $($banned -join ', ')"
}
Write-Host "==> $Path ($label)"
$imports | ForEach-Object { Write-Host "    $_" }
