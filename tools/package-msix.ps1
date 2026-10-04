#Requires -Version 7
<#
.SYNOPSIS
  Builds an UNSIGNED MSIX of FastPDF: dist/FastPDF-<X.Y.Z.R>-x64.msix (+ .sha256). ADR 0010.

.DESCRIPTION
  1. tools/package.ps1 -StageOnly -Flavor msix: the same dist build, VERSIONINFO / PE resource /
     build-path checks and file set (exe, README, THIRD_PARTY_LICENSES.md, licenses/ incl. the
     third-party bundle, BUILDINFO.txt) as the zip, staged under <OutDir>/.msix-work
  2. layout = staged files + Assets/*.png (tools/icon/make_icon.py) + AppxManifest.xml filled
     from packaging/msix/AppxManifest.xml (four-part version from Cargo, Publisher placeholder);
     every layout file gets the source date as its file time (see Reproducibility)
  3. makeappx pack (Windows SDK) with full semantic validation (no /nv)
  4. round trip: makeappx unpack, then every layout file must come back byte-identical, the
     only extra file allowed is the package footprint (AppxBlockMap.xml, required;
     [Content_Types].xml, which makeappx unpack does not write), there must be no
     AppxSignature.p7x (unsigned), and the unpacked manifest must carry the expected identity,
     entry point, .pdf file type association, execution alias and runFullTrust
  5. removes <OutDir>/.msix-work unless -KeepWork

  Reproducibility: makeappx orders the entries by file time and stamps every entry with the
  time of packing (no option changes that). With the layout's file times set to the source
  date, two packages of the same staged files hold the same entries in the same order, with
  the same AppxBlockMap.xml and [Content_Types].xml; only the entry time stamps differ, so the
  .msix hash changes on every run (docs/RELEASE.md section 1.6). The block map hash printed at
  the end identifies the contents.

  It never signs, never installs (no signtool, no certificates, no Add-AppxPackage) and
  never enables Developer Mode. Installing needs the owner's signature: replace the
  -Publisher placeholder with the signing certificate's exact subject, sign, then test on a
  clean machine (docs/RELEASE.md section 4).

.EXAMPLE
  pwsh -File tools/package-msix.ps1
  pwsh -File tools/package-msix.ps1 -SkipBuild -KeepWork
  pwsh -File tools/package-msix.ps1 -Publisher 'CN=Example Ltd, O=Example Ltd, C=TW' -PublisherDisplayName 'Example Ltd'
#>
[CmdletBinding()]
param(
    [string]$CargoProfile = 'dist',
    [switch]$SkipBuild,
    [switch]$NoRemapPaths,
    [string]$OutDir,
    # Must equal the code-signing certificate's subject before the package can be signed.
    [string]$Publisher = 'CN=FASTPDF-UNSIGNED-PLACEHOLDER-REPLACE-WITH-CERT-SUBJECT',
    [string]$PublisherDisplayName = 'FastPDF (unsigned build)',
    # Fourth version field. Required for Cargo pre-release versions (MSIX has no pre-release field).
    [int]$Revision = 0,
    [string]$MakeAppx,
    [switch]$KeepWork
)

$ErrorActionPreference = 'Stop'
$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $OutDir) { $OutDir = Join-Path $Repo 'dist' }
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
$Work = Join-Path $OutDir '.msix-work'
$TemplateDir = Join-Path $Repo 'packaging\msix'

function Step([string]$text) { Write-Host "==> $text" -ForegroundColor Cyan }
function Get-Sha256([string]$path) { (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant() }
function Get-RelativeFiles([string]$root) {
    $prefix = $root.TrimEnd('\') + '\'
    Get-ChildItem -LiteralPath $root -Recurse -File | ForEach-Object { $_.FullName.Substring($prefix.Length) } | Sort-Object
}

if ($Publisher -notmatch '^CN=[^,]+') { throw "-Publisher must be an X.500 subject starting with CN= (got '$Publisher')" }
if ($Revision -lt 0 -or $Revision -gt 65535) { throw '-Revision must be 0..65535' }

# ------------------------------------------------------------------ makeappx
if (-not $MakeAppx) {
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $MakeAppx = Get-ChildItem -Path $kits -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^\d+(\.\d+){3}$' -and (Test-Path (Join-Path $_.FullName 'x64\makeappx.exe')) } |
        Sort-Object { [version]$_.Name } -Descending |
        Select-Object -First 1 |
        ForEach-Object { Join-Path $_.FullName 'x64\makeappx.exe' }
}
if (-not $MakeAppx -or -not (Test-Path $MakeAppx)) {
    throw 'makeappx.exe not found: install the Windows SDK (it ships with the VS Build Tools C++ workload) or pass -MakeAppx'
}
Step "makeappx: $MakeAppx"

# ------------------------------------------------------------------ stage (shared with the zip)
if (Test-Path $Work) { Remove-Item -Recurse -Force $Work }
New-Item -ItemType Directory -Force $Work | Out-Null
$stageArgs = @{ StageOnly = $true; Flavor = 'msix'; OutDir = $Work; CargoProfile = $CargoProfile }
if ($SkipBuild) { $stageArgs.SkipBuild = $true }
if ($NoRemapPaths) { $stageArgs.NoRemapPaths = $true }
$staged = & (Join-Path $PSScriptRoot 'package.ps1') @stageArgs |
    Where-Object { $_ -is [pscustomobject] -and $_.PSObject.Properties['stage'] } | Select-Object -Last 1
if (-not $staged) { throw 'tools/package.ps1 -StageOnly returned no staging folder' }
if ($staged.source_date -isnot [DateTimeOffset]) { throw 'tools/package.ps1 -StageOnly returned no source date' }

# ------------------------------------------------------------------ version
if ($staged.version -notmatch '^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$') {
    throw "unexpected Cargo version '$($staged.version)'"
}
$parts = @([int]$Matches[1], [int]$Matches[2], [int]$Matches[3])
if ($Matches[4] -and -not $PSBoundParameters.ContainsKey('Revision')) {
    throw "Cargo version $($staged.version) is a pre-release; MSIX versions have no pre-release field. Pass -Revision N (and keep versions increasing so updates install)."
}
if ($parts | Where-Object { $_ -gt 65535 }) { throw "version $($staged.version) does not fit MSIX fields (0..65535)" }
$Version4 = ($parts + $Revision) -join '.'
$Msix = Join-Path $OutDir "FastPDF-$Version4-x64.msix"
Write-Host "  Cargo $($staged.version) -> MSIX $Version4"

# ------------------------------------------------------------------ layout
Step 'layout: staged files + Assets + AppxManifest.xml'
$Layout = Join-Path $Work 'layout'
Copy-Item -Recurse -LiteralPath $staged.stage -Destination $Layout
New-Item -ItemType Directory -Force (Join-Path $Layout 'Assets') | Out-Null
Copy-Item (Join-Path $TemplateDir 'Assets\*.png') (Join-Path $Layout 'Assets')

$esc = [System.Security.SecurityElement]
$manifestText = (Get-Content -Raw -LiteralPath (Join-Path $TemplateDir 'AppxManifest.xml') -Encoding utf8).
    Replace('{{VERSION}}', $Version4).
    Replace('{{PUBLISHER_DISPLAY_NAME}}', $esc::Escape($PublisherDisplayName)).
    Replace('{{PUBLISHER}}', $esc::Escape($Publisher)).
    Replace('{{ARCH}}', 'x64')
if ($manifestText -match '\{\{[A-Z_]+\}\}') { throw "unfilled placeholder in the manifest: $($Matches[0])" }
$manifestPath = Join-Path $Layout 'AppxManifest.xml'
[IO.File]::WriteAllText($manifestPath, $manifestText, [Text.UTF8Encoding]::new($false))
$xml = [xml]$manifestText   # well-formed or throw
$ns = [System.Xml.XmlNamespaceManager]::new($xml.NameTable)
$ns.AddNamespace('m', 'http://schemas.microsoft.com/appx/manifest/foundation/windows10')
$ns.AddNamespace('uap', 'http://schemas.microsoft.com/appx/manifest/uap/windows10')
foreach ($logo in @($xml.SelectSingleNode('/m:Package/m:Properties/m:Logo', $ns).InnerText) +
        @($xml.SelectNodes('//uap:VisualElements', $ns) | ForEach-Object { $_.Square150x150Logo; $_.Square44x44Logo })) {
    if (-not (Test-Path -LiteralPath (Join-Path $Layout $logo))) { throw "manifest references missing $logo" }
}
if (-not (Test-Path -LiteralPath (Join-Path $Layout 'fastpdf.exe'))) { throw 'layout has no fastpdf.exe' }
# makeappx drops empty folders silently, so the payload would differ from the layout unnoticed.
$emptyDirs = @(Get-ChildItem -LiteralPath $Layout -Recurse -Directory -Force |
    Where-Object { -not (Get-ChildItem -LiteralPath $_.FullName -Force | Select-Object -First 1) } |
    ForEach-Object { $_.FullName.Substring($Layout.Length + 1) })
if ($emptyDirs) { throw "empty folder(s) in the MSIX layout: $($emptyDirs -join ', ')" }
# makeappx orders the entries by file time; with one time for every file the order (and so
# AppxBlockMap.xml and [Content_Types].xml) depends on the files only, not on when they were copied.
$layoutTime = $staged.source_date.UtcDateTime
Get-ChildItem -LiteralPath $Layout -Recurse -File | ForEach-Object { $_.LastWriteTimeUtc = $layoutTime }
$layoutFiles = @(Get-RelativeFiles $Layout)
Write-Host "  $($layoutFiles.Count) files, file times set to the source date $($layoutTime.ToString('yyyy-MM-dd HH:mm:ss')) UTC"

# ------------------------------------------------------------------ pack
Step "makeappx pack -> $Msix"
if (Test-Path $Msix) { Remove-Item -Force $Msix }
$packOutput = & $MakeAppx pack /o /h SHA256 /d $Layout /p $Msix 2>&1 | Out-String
if ($LASTEXITCODE -ne 0) { throw "makeappx pack failed ($LASTEXITCODE):`n$packOutput" }
# makeappx lists every payload file; show the rest (banner, warnings, result) and a count.
$packLines = @($packOutput -split "`r?`n" | Where-Object { $_.Trim() })
$payloadCount = @($packLines | Where-Object { $_ -match 'as a payload file' }).Count
$packSummary = @($packLines | Where-Object { $_ -notmatch 'as a payload file' })
$packSummary | ForEach-Object { Write-Host "  $_" }
Write-Host "  ($payloadCount payload files + AppxManifest.xml)"
$msixHash = Get-Sha256 $Msix
"$msixHash  $([IO.Path]::GetFileName($Msix))" | Set-Content "$Msix.sha256" -Encoding ascii -NoNewline

# ------------------------------------------------------------------ round trip
Step 'round trip: makeappx unpack + compare'
$Unpacked = Join-Path $Work 'unpacked'
$unpackOutput = & $MakeAppx unpack /o /p $Msix /d $Unpacked 2>&1 | Out-String
if ($LASTEXITCODE -ne 0) { throw "makeappx unpack failed ($LASTEXITCODE):`n$unpackOutput" }
$unpackedFiles = @(Get-RelativeFiles $Unpacked)
$footprint = @('AppxBlockMap.xml', '[Content_Types].xml')   # [Content_Types].xml is optional: unpack omits it
if ($unpackedFiles -contains 'AppxSignature.p7x') { throw 'the package is signed (AppxSignature.p7x); this script must only produce unsigned packages' }
$missing = @($layoutFiles | Where-Object { $unpackedFiles -notcontains $_ })
$extra = @($unpackedFiles | Where-Object { $layoutFiles -notcontains $_ -and $footprint -notcontains $_ })
if ($missing -or $extra) { throw "round trip mismatch. missing: $($missing -join ', ') extra: $($extra -join ', ')" }
if ($unpackedFiles -notcontains 'AppxBlockMap.xml') { throw 'unpacked package has no AppxBlockMap.xml' }
$changed = @($layoutFiles | Where-Object { (Get-Sha256 (Join-Path $Layout $_)) -ne (Get-Sha256 (Join-Path $Unpacked $_)) })
if ($changed) { throw "files changed by the round trip: $($changed -join ', ')" }

$m = [xml](Get-Content -Raw -LiteralPath (Join-Path $Unpacked 'AppxManifest.xml') -Encoding utf8)
$ns = [System.Xml.XmlNamespaceManager]::new($m.NameTable)
$ns.AddNamespace('m', 'http://schemas.microsoft.com/appx/manifest/foundation/windows10')
$ns.AddNamespace('uap', 'http://schemas.microsoft.com/appx/manifest/uap/windows10')
$ns.AddNamespace('uap5', 'http://schemas.microsoft.com/appx/manifest/uap/windows10/5')
$ns.AddNamespace('rescap', 'http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities')
$identity = $m.SelectSingleNode('/m:Package/m:Identity', $ns)
$app = $m.SelectSingleNode('/m:Package/m:Applications/m:Application', $ns)
$checks = [ordered]@{
    'Identity Version'              = $identity.Version -eq $Version4
    'Identity Publisher'            = $identity.Publisher -eq $Publisher
    'Identity Name / architecture'  = $identity.Name -eq 'FastPDF' -and $identity.ProcessorArchitecture -eq 'x64'
    'full-trust entry point'        = $app.Executable -eq 'fastpdf.exe' -and $app.EntryPoint -eq 'Windows.FullTrustApplication'
    '.pdf file type association'    = [bool]$m.SelectSingleNode("//uap:FileTypeAssociation/uap:SupportedFileTypes/uap:FileType[.='.pdf']", $ns)
    'fastpdf.exe execution alias'   = [bool]$m.SelectSingleNode("//uap5:AppExecutionAlias/uap5:ExecutionAlias[@Alias='fastpdf.exe']", $ns)
    'runFullTrust capability'       = [bool]$m.SelectSingleNode("//rescap:Capability[@Name='runFullTrust']", $ns)
    'no other capabilities'         = @($m.SelectNodes('/m:Package/m:Capabilities/*', $ns)).Count -eq 1
}
$failed = @($checks.GetEnumerator() | Where-Object { -not $_.Value } | ForEach-Object { $_.Key })
if ($failed) { throw "unpacked manifest check failed: $($failed -join ', ')" }
$blockMapFiles = @(([xml](Get-Content -Raw -LiteralPath (Join-Path $Unpacked 'AppxBlockMap.xml'))).BlockMap.File).Count
# Same staged files -> same block map (file and block hashes, in package order), unlike the .msix hash.
$blockMapHash = Get-Sha256 (Join-Path $Unpacked 'AppxBlockMap.xml')
Write-Host "  $($layoutFiles.Count) payload files byte-identical; block map lists $blockMapFiles; no signature; manifest: $($checks.Keys -join ', ')"

# ------------------------------------------------------------------ cleanup + summary
if (-not $KeepWork) { Remove-Item -Recurse -Force $Work }
Step 'done (unsigned: cannot be installed until the owner signs it, see docs/RELEASE.md section 4)'
$size = (Get-Item $Msix).Length
Write-Host "  package : $Msix ($([math]::Round($size / 1MB, 2)) MB)"
Write-Host "  sha256  : $msixHash (differs on every run: makeappx stamps the entries with the packing time)"
Write-Host "  blockmap: $blockMapHash (AppxBlockMap.xml; the same for the same staged files)"
[pscustomobject]@{
    version = $staged.version; msix_version = $Version4; msix = $Msix; bytes = $size; sha256 = $msixHash
    blockmap_sha256 = $blockMapHash
    exe_sha256 = $staged.exe_sha256; payload_files = $layoutFiles.Count; makeappx = $MakeAppx
    payload_files_packed = $payloadCount; pack_output = ($packSummary -join "`n")
    work = if ($KeepWork) { $Work } else { $null }
}
