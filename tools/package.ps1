#Requires -Version 7
<#
.SYNOPSIS
  Builds the portable FastPDF release: dist/FastPDF-<version>-win-x64.zip (+ .sha256).

.DESCRIPTION
  1. cargo build --profile dist -p fastpdf-app --locked   (honors CARGO_TARGET_DIR)
     with --remap-path-prefix for CARGO_HOME and the repo (unless -NoRemapPaths), so
     dependency panic locations do not embed the build account's user name; the
     binary is then checked for leftover CARGO_HOME / USERPROFILE / repo paths.
     The linker gets /Brepro: no link time stamp and no random PDB GUID in the exe, so
     the same sources and toolchain give the same exe (docs/RELEASE.md section 1.6)
  2. checks the exe's VERSIONINFO against the Cargo version (build.rs ran) and its PE
     resources: RT_GROUP_ICON #1, the icon images, RT_VERSION #1 and exactly one
     RT_MANIFEST (GPUI's; a second one would mean a resource conflict)
  3. stages fastpdf.exe, README.md, THIRD_PARTY_LICENSES.md, the top-level files of licenses/
     (subfolders such as licenses/overrides/ are inputs of the bundle step, not shipped as is),
     BUILDINFO.txt and licenses/third-party/: each linked crate's own license and NOTICE files
     plus MISSING.md (python tools/license_report.py --bundle; needs python and cargo on PATH);
     the staging folder may not contain empty folders. BUILDINFO.txt records the commit,
     whether the working tree differs from it (uncommitted changes, or line endings that git
     status hides) and the source date (SOURCE_DATE_EPOCH if set, else the HEAD commit time),
     never the packaging time
  4. zips the staging folder (top-level folder inside the zip), writes <zip>.sha256.
     The zip is reproducible: entries sorted by path (ordinal), each stamped with the
     source date, no file attributes, no directory entries, so the same exe and commit
     give the same zip bytes (with the same PowerShell/.NET, whose zlib does the deflate)
  5. verifies the zip: exact entry list (under licenses/third-party/ exactly the files the
     bundle step reported writing), no directory entries, sorted entries stamped with the
     source date, and the exe's SHA-256 inside the zip
  6. smoke test on the extracted copy: --version and --help (no window), then one
     GUI start with FASTPDF_BENCH=1 on a fixture until the first frame is presented;
     the process tree is always terminated. Saved settings and recent files are
     not touched (FASTPDF_SETTINGS_FILE / FASTPDF_RECENT_FILE are set empty).
  With -StageOnly the script stops after step 3 and returns the staging folder: that
  is how tools/package-msix.ps1 reuses the same build, checks and file set.
  The script writes only under -OutDir (default dist/) and the cargo target dir.
  See docs/RELEASE.md for the full release checklist.

.EXAMPLE
  pwsh -File tools/package.ps1
  pwsh -File tools/package.ps1 -SkipBuild -NoGuiSmoke
  pwsh -File tools/package.ps1 -StageOnly -OutDir dist/.work -Flavor msix
#>
[CmdletBinding()]
param(
    [string]$CargoProfile = 'dist',
    [switch]$SkipBuild,
    [switch]$NoRemapPaths,
    [string]$OutDir,
    [switch]$NoSmokeTest,
    [switch]$NoGuiSmoke,
    [string]$SmokePdf,
    [int]$SmokeTimeoutSec = 30,
    # Build, check and stage only (no zip, no smoke test); returns the staging folder.
    [switch]$StageOnly,
    # Written to BUILDINFO.txt: 'portable' (zip) or 'msix'.
    [ValidateSet('portable', 'msix')][string]$Flavor = 'portable'
)

$ErrorActionPreference = 'Stop'
$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $OutDir) { $OutDir = Join-Path $Repo 'dist' }
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem

function Step([string]$text) { Write-Host "==> $text" -ForegroundColor Cyan }
function Invoke-Checked([string]$exe, [string[]]$arguments) {
    & $exe @arguments
    if ($LASTEXITCODE -ne 0) { throw "$exe $($arguments -join ' ') failed with exit code $LASTEXITCODE" }
}

# ------------------------------------------------------------------ version
Push-Location $Repo
try {
    $meta = cargo metadata --no-deps --format-version 1 --offline | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed' }
} finally { Pop-Location }
$app = $meta.packages | Where-Object name -eq 'fastpdf-app'
if (-not $app) { throw 'fastpdf-app not found in the workspace' }
$Version = $app.version
$Name = "FastPDF-$Version-win-x64"
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Repo 'target' }
$Exe = Join-Path $TargetDir "$CargoProfile\fastpdf.exe"

# ------------------------------------------------------------------ build
if (-not $SkipBuild) {
    $cargoArgs = @('build', '--profile', $CargoProfile, '-p', 'fastpdf-app', '--locked')
    # /Brepro: link.exe writes a hash of the image instead of the link time (PE header, debug
    # directory) and derives the PDB GUID from it, so rebuilding the same sources gives the same exe.
    $rustflags = @('-Clink-arg=/Brepro')
    if (-not $NoRemapPaths) {
        $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
        $remap = @("--remap-path-prefix=$cargoHome=cargo-home", "--remap-path-prefix=$Repo=fastpdf")
        if ($remap -match "'") { throw "a path contains ' (cannot be a TOML literal string); use -NoRemapPaths" }
        $rustflags += $remap
    }
    # Joined with .cargo/config.toml's target.'cfg(windows)'.rustflags (Cargo merges
    # target.<triple> and target.<cfg> flags; RUSTFLAGS would replace them instead).
    $cargoArgs += @('--config', "target.x86_64-pc-windows-msvc.rustflags=[$(($rustflags | ForEach-Object { "'$_'" }) -join ', ')]")
    Step "cargo $($cargoArgs -join ' ')"
    Push-Location $Repo
    try { Invoke-Checked 'cargo' $cargoArgs } finally { Pop-Location }
}
if (-not (Test-Path $Exe)) { throw "missing $Exe (build first or check CARGO_TARGET_DIR)" }

Step 'VERSIONINFO'
$vi = (Get-Item $Exe).VersionInfo
if ($vi.ProductName -ne 'FastPDF' -or $vi.ProductVersion -ne $Version -or $vi.FileVersion -ne $Version) {
    throw "unexpected version resource: ProductName='$($vi.ProductName)' ProductVersion='$($vi.ProductVersion)' FileVersion='$($vi.FileVersion)' (expected FastPDF $Version)"
}
if ($vi.CompanyName) { throw "CompanyName must stay empty, found '$($vi.CompanyName)'" }
Write-Host "  $($vi.ProductName) $($vi.ProductVersion) (file $($vi.FileMajorPart).$($vi.FileMinorPart).$($vi.FileBuildPart).$($vi.FilePrivatePart)), $([math]::Round((Get-Item $Exe).Length / 1MB, 2)) MB"

Step 'PE resources (icon, version, single manifest)'
if (-not ('FastPdfPeResources' -as [type])) {
    Add-Type -ReferencedAssemblies System.Runtime.InteropServices, System.Collections -TypeDefinition @'
using System; using System.Collections.Generic; using System.Runtime.InteropServices;
public static class FastPdfPeResources {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern IntPtr LoadLibraryExW(string path, IntPtr file, uint flags);
    [DllImport("kernel32.dll")] static extern bool FreeLibrary(IntPtr h);
    delegate bool EnumNameProc(IntPtr module, IntPtr type, IntPtr name, IntPtr param);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] static extern bool EnumResourceNamesW(IntPtr module, IntPtr type, EnumNameProc cb, IntPtr param);
    // Resource names of one type; the file is loaded as data only (no code runs).
    public static string[] Names(string path, int type) {
        IntPtr h = LoadLibraryExW(path, IntPtr.Zero, 0x2 | 0x20); // AS_DATAFILE | AS_IMAGE_RESOURCE
        if (h == IntPtr.Zero) throw new InvalidOperationException("LoadLibraryEx failed: Win32 error " + Marshal.GetLastWin32Error());
        var names = new List<string>();
        try {
            EnumResourceNamesW(h, new IntPtr(type), delegate (IntPtr m, IntPtr t, IntPtr n, IntPtr p) {
                long v = n.ToInt64();
                names.Add((v >> 16) == 0 ? "#" + v : Marshal.PtrToStringUni(n));
                return true;
            }, IntPtr.Zero);
        } finally { FreeLibrary(h); }
        return names.ToArray();
    }
}
'@
}
$res = [ordered]@{
    RT_GROUP_ICON = @([FastPdfPeResources]::Names($Exe, 14)); RT_ICON = @([FastPdfPeResources]::Names($Exe, 3))
    RT_VERSION = @([FastPdfPeResources]::Names($Exe, 16)); RT_MANIFEST = @([FastPdfPeResources]::Names($Exe, 24))
}
if ($res.RT_GROUP_ICON -notcontains '#1') { throw "no RT_GROUP_ICON #1 (icon not embedded): $($res.RT_GROUP_ICON -join ',')" }
if ($res.RT_ICON.Count -lt 6) { throw "expected 6 icon images (16-256 px), found $($res.RT_ICON.Count)" }
if ($res.RT_VERSION -notcontains '#1') { throw 'no RT_VERSION #1' }
if ($res.RT_MANIFEST.Count -ne 1) { throw "expected exactly one RT_MANIFEST (GPUI's), found: $($res.RT_MANIFEST -join ',')" }
Write-Host "  RT_GROUP_ICON $($res.RT_GROUP_ICON -join ','); RT_ICON x$($res.RT_ICON.Count); RT_VERSION $($res.RT_VERSION -join ','); RT_MANIFEST $($res.RT_MANIFEST -join ',')"

if (-not $NoRemapPaths -and -not $SkipBuild) {
    Step 'privacy: no build-machine paths in the binary'
    $latin1 = [Text.Encoding]::GetEncoding(28591)
    $haystack = $latin1.GetString([IO.File]::ReadAllBytes($Exe))
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
    foreach ($needle in @($cargoHome, $env:USERPROFILE, $Repo) | Select-Object -Unique) {
        $hits = 0; $at = 0
        $bytes = $latin1.GetString([Text.Encoding]::UTF8.GetBytes($needle))
        while (($at = $haystack.IndexOf($bytes, $at, [StringComparison]::OrdinalIgnoreCase)) -ge 0) { $hits++; $at += $bytes.Length }
        if ($hits) { throw "fastpdf.exe still contains '$needle' $hits time(s)" }
    }
    Write-Host '  none of CARGO_HOME, USERPROFILE, repo path found'
}

# ------------------------------------------------------------------ stage
Step "staging $Name"
$Stage = Join-Path $OutDir $Name
if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
New-Item -ItemType Directory -Force (Join-Path $Stage 'licenses') | Out-Null
Copy-Item $Exe (Join-Path $Stage 'fastpdf.exe')
Copy-Item (Join-Path $Repo 'README.md') $Stage
Copy-Item (Join-Path $Repo 'THIRD_PARTY_LICENSES.md') $Stage
# Top-level files only, matching the expected entry list below: subfolders (licenses/overrides/)
# feed license_report.py --bundle, which writes what ships into licenses/third-party/.
Get-ChildItem -LiteralPath (Join-Path $Repo 'licenses') -File | Copy-Item -Destination (Join-Path $Stage 'licenses')

Step 'third-party license texts -> licenses/third-party/ (license_report.py --bundle)'
$ThirdParty = Join-Path $Stage 'licenses\third-party'
$bundleList = Join-Path $OutDir "$Name.third-party-files.txt"
Push-Location $Repo
try {
    python tools/license_report.py --bundle $ThirdParty --bundle-list $bundleList | ForEach-Object { Write-Host "  $_" }
    if ($LASTEXITCODE -ne 0) { throw "tools/license_report.py --bundle failed with exit code $LASTEXITCODE" }
    # Paths relative to licenses/third-party/, exactly as the bundle step wrote them.
    $bundleFiles = @(Get-Content -LiteralPath $bundleList -Encoding utf8 | Where-Object { $_ })
} finally {
    Pop-Location
    Remove-Item -LiteralPath $bundleList -Force -ErrorAction SilentlyContinue
}
if ($bundleFiles -notcontains 'MISSING.md') { throw 'license_report.py --bundle did not report MISSING.md' }

$exeHash = (Get-FileHash -Algorithm SHA256 $Exe).Hash.ToLowerInvariant()
$gitRev = git -C $Repo rev-parse HEAD 2>$null
$dirty = $false
$commitEpoch = $null
$eolDrift = @()
if ($gitRev) {
    $dirty = (git -C $Repo status --porcelain 2>$null | Measure-Object).Count -gt 0
    $commitEpoch = git -C $Repo log -1 --format=%ct HEAD 2>$null
    # git status hides line-ending-only differences (.gitattributes: text=auto eol=lf), but the
    # package ships bytes: a tracked file with CRLF where a fresh checkout has LF (git ls-files
    # --eol: i/lf w/crlf) makes the zip differ from one packaged in a clean clone.
    $eolDrift = @(git -C $Repo ls-files --eol 2>$null | ForEach-Object {
        if ($_ -match '^i/(\S+)\s+w/(\S+)\s.*\t(.+)$' -and $Matches[1] -ne $Matches[2]) { $Matches[3] }
    })
    if ($eolDrift) {
        Write-Warning ("line endings of $($eolDrift.Count) tracked file(s) differ from a fresh checkout, which " +
            "git status does not show, so the package may differ from one built in a clean clone: " +
            "$(($eolDrift | Select-Object -First 5) -join ', ')$(if ($eolDrift.Count -gt 5) { ', ...' }). " +
            'Package from a fresh clone, or delete these files and run git checkout -- on them.')
    }
} else { $gitRev = 'unknown (not a git checkout)' }
$treeNotes = @()
if ($dirty) { $treeNotes += 'working tree had uncommitted changes' }
if ($eolDrift) { $treeNotes += "line endings of $($eolDrift.Count) tracked file(s) differ from a fresh checkout" }
# Source date (reproducible-builds.org): SOURCE_DATE_EPOCH if set, else the HEAD commit time.
# BUILDINFO.txt and every zip entry carry it instead of the clock, so packaging the same exe
# and commit twice gives the same bytes.
if ($env:SOURCE_DATE_EPOCH) {
    if ($env:SOURCE_DATE_EPOCH -notmatch '^\d{1,11}$') {
        throw "SOURCE_DATE_EPOCH must be whole seconds since 1970-01-01 UTC, got '$env:SOURCE_DATE_EPOCH'"
    }
    $sourceEpoch = [long]$env:SOURCE_DATE_EPOCH; $sourceDateOrigin = 'SOURCE_DATE_EPOCH'
} elseif ($commitEpoch -match '^\d+$') {
    $sourceEpoch = [long]$commitEpoch; $sourceDateOrigin = 'commit time'
} else {
    Write-Warning 'no source date (not a git checkout and SOURCE_DATE_EPOCH is not set): using 1980-01-01'
    $sourceEpoch = [long]0; $sourceDateOrigin = 'unknown: not a git checkout and SOURCE_DATE_EPOCH not set'
}
# Zip entry times (DOS format) run from 1980 to 2107 in 2-second steps.
$SourceDate = [DateTimeOffset]::FromUnixTimeSeconds([math]::Max($sourceEpoch, [long]315532800))
if ($SourceDate.Year -gt 2107) { throw "source date $($SourceDate.ToString('u')) does not fit a zip entry time (1980-2107)" }
# Inside the repo, so rust-toolchain.toml selects the toolchain that built the exe.
Push-Location $Repo
try { $rustc = (rustc -V 2>$null) } finally { Pop-Location }
@(
    "FastPDF $Version (win-x64, $Flavor)"
    "git: $gitRev$(if ($treeNotes) { " ($($treeNotes -join '; '))" })"
    # Not where the date came from: SOURCE_DATE_EPOCH set to the commit time must give the same zip.
    "source date: $($SourceDate.UtcDateTime.ToString('yyyy-MM-dd HH:mm:ss')) UTC"
    "rustc: $rustc"
    "cargo profile: $CargoProfile$(if ($SkipBuild) { ' (existing build; path remapping not verified)' } elseif ($NoRemapPaths) { ' (paths not remapped; link /Brepro)' } else { ' (--remap-path-prefix: CARGO_HOME, repo; link /Brepro)' })"
    "fastpdf.exe sha256: $exeHash"
) | Set-Content (Join-Path $Stage 'BUILDINFO.txt') -Encoding utf8

# An empty folder would become a stray directory entry in the zip (an MSIX drops it silently).
$emptyDirs = @(Get-ChildItem -LiteralPath $Stage -Recurse -Directory -Force |
    Where-Object { -not (Get-ChildItem -LiteralPath $_.FullName -Force | Select-Object -First 1) } |
    ForEach-Object { $_.FullName.Substring($Stage.Length + 1) })
if ($emptyDirs) { throw "empty folder(s) in the staging folder: $($emptyDirs -join ', ')" }

if ($StageOnly) {
    Step "staged $Stage"
    return [pscustomobject]@{
        version = $Version; name = $Name; stage = $Stage; exe = $Exe; exe_sha256 = $exeHash
        third_party_files = $bundleFiles; source_date = $SourceDate
    }
}

# ------------------------------------------------------------------ zip + hash
$Zip = Join-Path $OutDir "$Name.zip"
Step "zip -> $Zip"
if (Test-Path $Zip) { Remove-Item -Force $Zip }
# Reproducible zip: files only, in ordinal path order, each stamped with the source date (the
# DOS time holds its UTC clock time) and no file attributes. Nothing depends on the clock, the
# time zone, file times or file system order; the deflate bytes come from .NET's zlib, so they
# depend on the PowerShell/.NET version (printed below).
$zipFiles = [string[]]@(Get-ChildItem -LiteralPath $Stage -Recurse -File -Force |
    ForEach-Object { $_.FullName.Substring($Stage.Length + 1).Replace('\', '/') })
[Array]::Sort($zipFiles, [StringComparer]::Ordinal)
$archive = [System.IO.Compression.ZipFile]::Open($Zip, [System.IO.Compression.ZipArchiveMode]::Create)
try {
    foreach ($rel in $zipFiles) {
        $entry = $archive.CreateEntry("$Name/$rel", [System.IO.Compression.CompressionLevel]::Optimal)
        $entry.LastWriteTime = $SourceDate
        $entry.ExternalAttributes = 0
        $in = [IO.File]::OpenRead((Join-Path $Stage $rel))
        try {
            $out = $entry.Open()
            try { $in.CopyTo($out) } finally { $out.Dispose() }
        } finally { $in.Dispose() }
    }
} finally { $archive.Dispose() }
Write-Host "  $($zipFiles.Count) entries dated $($SourceDate.UtcDateTime.ToString('yyyy-MM-dd HH:mm:ss')) UTC ($sourceDateOrigin); PowerShell $($PSVersionTable.PSVersion), .NET $([Environment]::Version)"
$zipHash = (Get-FileHash -Algorithm SHA256 $Zip).Hash.ToLowerInvariant()
"$zipHash  $Name.zip" | Set-Content "$Zip.sha256" -Encoding ascii -NoNewline

# ------------------------------------------------------------------ verify zip
Step 'verify zip contents'
$expected = @("$Name/fastpdf.exe", "$Name/README.md", "$Name/THIRD_PARTY_LICENSES.md", "$Name/BUILDINFO.txt") +
    @(Get-ChildItem (Join-Path $Repo 'licenses') -File | ForEach-Object { "$Name/licenses/$($_.Name)" }) +
    @($bundleFiles | ForEach-Object { "$Name/licenses/third-party/$_" })
$archive = [System.IO.Compression.ZipFile]::OpenRead($Zip)
try {
    $names = @($archive.Entries | ForEach-Object { $_.FullName.Replace('\', '/') })
    # The zip holds files only: a directory entry would be a stray folder.
    $dirEntries = @($names | Where-Object { $_.EndsWith('/') })
    if ($dirEntries) { throw "unexpected directory entries in the zip: $($dirEntries -join ', ')" }
    # Reproducibility: ordinal order, the source date (2 s resolution) and no attributes.
    $sorted = [string[]]$names.Clone()
    [Array]::Sort($sorted, [StringComparer]::Ordinal)
    if (($sorted -join "`n") -cne ($names -join "`n")) { throw 'zip entries are not in ordinal path order' }
    $dosClock = $SourceDate.UtcDateTime.AddSeconds(-($SourceDate.UtcDateTime.Second % 2))
    $unstable = @($archive.Entries | Where-Object { $_.LastWriteTime.DateTime -ne $dosClock -or $_.ExternalAttributes -ne 0 } |
        ForEach-Object { $_.FullName })
    if ($unstable) { throw "zip entries without the source date or with file attributes: $($unstable -join ', ')" }
    $entries = $names
    $missing = @($expected | Where-Object { $entries -notcontains $_ })
    $extra = @($entries | Where-Object { $expected -notcontains $_ })
    if ($missing -or $extra) { throw "zip mismatch. missing: $($missing -join ', ') extra: $($extra -join ', ')" }
    $entry = $archive.GetEntry("$Name/fastpdf.exe")
    $stream = $entry.Open()
    try {
        $sha = [System.Security.Cryptography.SHA256]::Create()
        $inZip = ([BitConverter]::ToString($sha.ComputeHash($stream)) -replace '-', '').ToLowerInvariant()
    } finally { $stream.Dispose() }
    if ($inZip -ne $exeHash) { throw "fastpdf.exe in the zip differs from the build ($inZip vs $exeHash)" }
} finally { $archive.Dispose() }
Write-Host "  $($entries.Count) files ($($bundleFiles.Count) under licenses/third-party/), sorted and dated, exe hash matches"

# ------------------------------------------------------------------ smoke test
$smoke = [ordered]@{}
if (-not $NoSmokeTest) {
    $SmokeDir = Join-Path $OutDir ".smoke-$Name"
    if (Test-Path $SmokeDir) { Remove-Item -Recurse -Force $SmokeDir }
    [System.IO.Compression.ZipFile]::ExtractToDirectory($Zip, $SmokeDir)
    $SmokeExe = Join-Path $SmokeDir "$Name\fastpdf.exe"
    try {
        Step 'smoke: --version / --help (no window)'
        function Invoke-Cli([string[]]$cliArgs) {
            $psi = New-Object System.Diagnostics.ProcessStartInfo $SmokeExe
            foreach ($a in $cliArgs) { $psi.ArgumentList.Add($a) }
            $psi.UseShellExecute = $false; $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
            # Never read or write the user's settings / recent files, even for CLI-only runs.
            $psi.Environment['FASTPDF_SETTINGS_FILE'] = ''
            $psi.Environment['FASTPDF_RECENT_FILE'] = ''
            $p = [System.Diagnostics.Process]::Start($psi)
            $out = $p.StandardOutput.ReadToEndAsync(); $err = $p.StandardError.ReadToEndAsync()
            if (-not $p.WaitForExit(15000)) { $p.Kill($true); throw "fastpdf $cliArgs did not exit" }
            [pscustomobject]@{ code = $p.ExitCode; out = $out.Result.Trim(); err = $err.Result.Trim() }
        }
        $v = Invoke-Cli @('--version')
        if ($v.code -ne 0 -or $v.out -ne "fastpdf $Version") { throw "--version: exit $($v.code), output '$($v.out)' $($v.err)" }
        $h = Invoke-Cli @('--help')
        if ($h.code -ne 0 -or $h.out -notmatch '^usage: fastpdf') { throw "--help: exit $($h.code), output '$($h.out)' $($h.err)" }
        $smoke.version = $v.out
        $smoke.help_first_line = ($h.out -split "`r?`n")[0]
        Write-Host "  $($v.out); $($smoke.help_first_line)"

        if (-not $NoGuiSmoke) {
            if (-not $SmokePdf) {
                $SmokePdf = Join-Path $Repo 'fixtures\generated\small-text\three-pages-platypus-times.pdf'
            }
            if (-not (Test-Path $SmokePdf)) {
                Write-Warning "no smoke fixture at $SmokePdf (uv run tools/fixtures/generate.py); GUI smoke skipped"
            } else {
                Step "smoke: GUI start with $([IO.Path]::GetFileName($SmokePdf)) (FASTPDF_BENCH=1)"
                $psi = New-Object System.Diagnostics.ProcessStartInfo $SmokeExe
                $psi.ArgumentList.Add((Resolve-Path $SmokePdf).Path)
                $psi.UseShellExecute = $false; $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
                $psi.Environment['FASTPDF_BENCH'] = '1'
                $psi.Environment['FASTPDF_SETTINGS_FILE'] = ''
                $psi.Environment['FASTPDF_RECENT_FILE'] = ''
                $sw = [System.Diagnostics.Stopwatch]::StartNew()
                $p = [System.Diagnostics.Process]::Start($psi)
                $errTask = $p.StandardError.ReadToEndAsync()
                $events = [ordered]@{}
                try {
                    $pending = $null
                    $firstPaintAt = $null
                    # Done at first_page_exact; after first_paint wait at most 10 s more for it.
                    while ($sw.Elapsed.TotalSeconds -lt $SmokeTimeoutSec -and -not $events.Contains('first_page_exact')) {
                        if ($events.Contains('first_paint') -and -not $firstPaintAt) { $firstPaintAt = $sw.Elapsed.TotalSeconds }
                        if ($firstPaintAt -and $sw.Elapsed.TotalSeconds - $firstPaintAt -gt 10) { break }
                        if (-not $pending) { $pending = $p.StandardOutput.ReadLineAsync() }
                        if (-not $pending.Wait(250)) { if ($p.HasExited) { break } else { continue } }
                        $line = $pending.Result; $pending = $null
                        if ($null -eq $line) { break }
                        try { $o = $line | ConvertFrom-Json -ErrorAction Stop; if ($o.event) { $events[[string]$o.event] = $o.t_ms } } catch { }
                    }
                } finally {
                    $alive = -not $p.HasExited
                    if ($alive) { $p.Kill($true); [void]$p.WaitForExit(5000) }
                }
                $smoke.gui_events_t_ms = $events
                $smoke.gui_exited_by_itself = -not $alive
                if (-not $events.Contains('first_paint')) {
                    throw "GUI smoke: no first_paint within $SmokeTimeoutSec s (events: $($events.Keys -join ', ')); stderr: $($errTask.Result)"
                }
                if (-not $alive) { throw "GUI smoke: fastpdf exited on its own (exit code $($p.ExitCode))" }
                Write-Host ("  events (ms from process start): " + (($events.GetEnumerator() | ForEach-Object { "$($_.Key) $($_.Value)" }) -join ', '))
            }
        }
    } finally {
        Start-Sleep -Milliseconds 300
        Remove-Item -Recurse -Force $SmokeDir -ErrorAction SilentlyContinue
    }
}

# ------------------------------------------------------------------ summary
Step 'done'
Write-Host "  package : $Zip ($([math]::Round((Get-Item $Zip).Length / 1MB, 2)) MB)"
Write-Host "  sha256  : $zipHash"
Write-Host "  exe     : $exeHash"
Write-Host "  source  : $gitRev, $($SourceDate.UtcDateTime.ToString('yyyy-MM-dd HH:mm:ss')) UTC ($sourceDateOrigin)"
[pscustomobject]@{
    version = $Version; zip = $Zip; sha256 = $zipHash; exe_sha256 = $exeHash; smoke = $smoke
    git = $gitRev; dirty = $dirty; eol_drift_files = $eolDrift.Count; source_date = $SourceDate
}
