<#
.SYNOPSIS
  FastPDF app-level KPI harness (SPEC §28-§29, PROJECT_AUDIT.md B-8).

.DESCRIPTION
  Launches a PDF reader N times and measures, from the outside:
    - time to window (first visible, non-cloaked top-level window of the launched process tree)
    - first non-blank frame, last visual change ("visually complete") and stability detection
      (PrintWindow captures; stable = StableFrames unchanged captures AND QuietMs without change)
    - idle working set / private bytes / CPU time delta / threads / handles (summed over the tree)
    - optional scroll / PageDown / zoom inputs (PostMessage to the app's own windows only)
    - FastPDF's own FASTPDF_BENCH=1 stdout JSON events, when the app emits them
  Results are merged into a JSON file under benchmarks/runs/ (git-ignored). See README.md.

.EXAMPLE
  pwsh -File tools/bench-app/bench-app.ps1 -Detect
  pwsh -File tools/bench-app/bench-app.ps1 -Preset upstream -Scenario launch-empty -Runs 3
  pwsh -File tools/bench-app/bench-app.ps1 -Preset upstream -Pdf fixtures/generated/large-text/dense-300p-times.pdf -ScrollNotches 20 -PageDowns 5
  pwsh -File tools/bench-app/bench-app.ps1 -Preset edge -Pdf fixtures/generated/large-text/dense-300p-times.pdf -WarmupRuns 1 -ScrollNotches 20 -PageDowns 5 -ZoomSteps 3
  pwsh -File tools/bench-app/bench-app.ps1 -Preset fastpdf -Pdf fixtures/generated/small-text/three-pages-platypus-times.pdf
  pwsh -File tools/bench-app/bench-app.ps1 -Exe C:\path\viewer.exe -Args '{pdf}' -Pdf some.pdf -Label myviewer
#>
[CmdletBinding()]
param(
    [ValidateSet('custom', 'fastpdf', 'upstream', 'edge', 'chrome', 'firefox', 'sumatra', 'acrobat')]
    [string]$Preset = 'custom',
    [string]$Exe,
    [Alias('Args')][string[]]$AppArgs,      # placeholders: {pdf} {pdf_uri} {profile}
    [string]$Pdf,
    [string]$Scenario,
    [string]$Label,
    [int]$Runs = 3,
    [int]$WarmupRuns = 0,
    [int]$IdleSeconds = 10,
    [int]$PostIdleSeconds = 5,
    [int]$StableFrames = 5,
    [int]$QuietMs = 1500,
    [int]$TimeoutSec = 30,
    [int]$ScrollNotches = 0,
    [int]$PageDowns = 0,
    [int]$ZoomSteps = 0,
    [ValidateSet('auto', 'wheel', 'keys')][string]$InputMode = 'auto',
    [switch]$CaptureStdout,
    [string]$OutFile,
    [string]$ShotsDir,
    [string]$ProfileRoot,
    [switch]$KeepProfiles,
    [switch]$AllowSharedProfile,             # needed for apps without an isolated profile (Acrobat)
    [string]$CacheState = 'warm',            # annotate 'cold-rammap' / 'cold-reboot' when you flushed the file cache yourself
    [string]$LaunchBudgetFile,
    [int]$MaxLaunches = 25,
    [switch]$NoScreenshots,
    [switch]$Detect,
    [switch]$Resummarize                      # recompute the summaries of an existing -OutFile (no launches)
)

$ErrorActionPreference = 'Stop'
$ToolVersion = '1.0.0'
$Schema = 'fastpdf-bench-app/1'
$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path

# ---------------------------------------------------------------- helper compile
if (-not ('FastPdfBench.Session' -as [type])) {
    $src = Get-Content -Raw (Join-Path $PSScriptRoot 'BenchApp.cs')
    if ($PSVersionTable.PSEdition -eq 'Desktop') {
        $refs = @('System', 'System.Core')
    } else {
        $refs = @('System.Runtime', 'System.Collections', 'System.Collections.NonGeneric', 'System.Diagnostics.Process',
            'System.ComponentModel.Primitives', 'System.IO.Compression', 'System.Runtime.InteropServices', 'System.Threading',
            'System.Threading.Thread', 'System.Text.Encoding.Extensions', 'System.Console', 'System.Linq')
    }
    Add-Type -TypeDefinition $src -ReferencedAssemblies $refs
}
[FastPdfBench.Native]::InitDpi()

# ---------------------------------------------------------------- app discovery
function Find-AppPath([string]$exeName, [string[]]$candidates) {
    foreach ($root in 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths', 'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths',
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\App Paths') {
        $k = Join-Path $root $exeName
        if (Test-Path $k) {
            $v = (Get-ItemProperty $k).'(default)'
            if ($v) { $v = $v.Trim('"'); if (Test-Path $v) { return $v } }
        }
    }
    foreach ($c in $candidates) { if ($c -and (Test-Path $c)) { return $c } }
    return $null
}

$pf = $env:ProgramFiles; $pf86 = ${env:ProgramFiles(x86)}; $la = $env:LOCALAPPDATA
$Known = [ordered]@{
    sumatra = @{ exe = 'SumatraPDF.exe'; cands = @("$la\SumatraPDF\SumatraPDF.exe", "$pf\SumatraPDF\SumatraPDF.exe", "$pf86\SumatraPDF\SumatraPDF.exe") }
    acrobat = @{ exe = 'Acrobat.exe'; alt = 'AcroRd32.exe'; cands = @("$pf\Adobe\Acrobat DC\Acrobat\Acrobat.exe", "$pf86\Adobe\Acrobat DC\Acrobat\Acrobat.exe",
            "$pf86\Adobe\Acrobat Reader DC\Reader\AcroRd32.exe", "$pf\Adobe\Acrobat Reader DC\Reader\AcroRd32.exe") }
    edge    = @{ exe = 'msedge.exe'; cands = @("$pf86\Microsoft\Edge\Application\msedge.exe", "$pf\Microsoft\Edge\Application\msedge.exe") }
    chrome  = @{ exe = 'chrome.exe'; cands = @("$pf\Google\Chrome\Application\chrome.exe", "$pf86\Google\Chrome\Application\chrome.exe", "$la\Google\Chrome\Application\chrome.exe") }
    firefox = @{ exe = 'firefox.exe'; cands = @("$pf\Mozilla Firefox\firefox.exe", "$pf86\Mozilla Firefox\firefox.exe", "$la\Mozilla Firefox\firefox.exe") }
}
function Resolve-KnownApp([string]$name) {
    $k = $Known[$name]
    $p = Find-AppPath $k.exe $k.cands
    if (-not $p -and $k.alt) { $p = Find-AppPath $k.alt @() }
    return $p
}

if ($Detect) {
    $rows = foreach ($name in $Known.Keys) {
        $p = Resolve-KnownApp $name
        [ordered]@{ app = $name; installed = [bool]$p; path = $p; version = if ($p) { (Get-Item $p).VersionInfo.ProductVersion } else { $null } }
    }
    $doc = [ordered]@{ schema = 'fastpdf-bench-app-detect/1'; generated = (Get-Date).ToString('o'); apps = @($rows) }
    $outDir = Join-Path $Repo 'benchmarks\runs'; New-Item -ItemType Directory -Force $outDir | Out-Null
    $doc | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $outDir 'app-detect.json') -Encoding utf8
    $rows | ForEach-Object { [pscustomobject]$_ } | Format-Table -AutoSize | Out-String | Write-Host
    return
}

# ---------------------------------------------------------------- presets
if ($Resummarize) {
    if (-not $OutFile -or -not (Test-Path $OutFile)) { throw "-Resummarize needs an existing -OutFile" }
    $script:ResummarizeOnly = $true
}
$ChromiumFlags = @('--user-data-dir={profile}', '--no-first-run', '--no-default-browser-check', '--disable-sync',
    '--disable-background-networking', '--disable-component-update', '--disable-default-apps', '--disable-domain-reliability',
    '--no-pings', '--metrics-recording-only', '--hide-crash-restore-bubble',
    '--disable-features=CalculateNativeWinOcclusion,Translate,OptimizationHints,MediaRouter',
    '--disable-backgrounding-occluded-windows', '--disable-renderer-backgrounding', '--disable-background-timer-throttling',
    '--window-size=1536,864', '--window-position=40,40', '--new-window', '{pdf_uri}')

$cfg = @{ template = @('{pdf}'); profile = 'none'; open = 'cli'; input = 'wheel'; close = 'close'; capture = $false; singleInstance = $null; shared = $false; zoom = $true; dialogTitle = $null }
switch ($Preset) {
    'upstream' {
        $cfg.exe = Join-Path $Repo 'upstream\pdf-reader-gpui\target\release\pdf-reader-gpui.exe'
        $cfg.template = @(); $cfg.open = 'dialog'; $cfg.dialogTitle = 'Open PDF file'; $cfg.close = 'kill'; $cfg.zoom = $false
    }
    'fastpdf' {
        $cands = @((Join-Path $Repo 'target\release\fastpdf.exe'), (Join-Path $Repo 'target\dist\fastpdf.exe'))
        $cfg.exe = ($cands | Where-Object { Test-Path $_ } | Select-Object -First 1); if (-not $cfg.exe) { $cfg.exe = $cands[0] }
        $cfg.capture = $true
    }
    { $_ -in 'edge', 'chrome' } {
        $cfg.exe = Resolve-KnownApp $Preset; $cfg.template = $ChromiumFlags; $cfg.profile = 'chromium'; $cfg.input = 'chromium'
    }
    'firefox' {
        $cfg.exe = Resolve-KnownApp 'firefox'; $cfg.profile = 'firefox'; $cfg.input = 'chromium'
        $cfg.template = @('-profile', '{profile}', '-no-remote', '-new-instance', '-width', '1536', '-height', '864', '{pdf_uri}')
    }
    'sumatra' {
        $cfg.exe = Resolve-KnownApp 'sumatra'; $cfg.profile = 'sumatra'; $cfg.singleInstance = 'SumatraPDF'; $cfg.input = 'keys'
        $cfg.template = @('-appdata', '{profile}', '{pdf}')
    }
    'acrobat' {
        $cfg.exe = Resolve-KnownApp 'acrobat'; $cfg.singleInstance = @('Acrobat', 'AcroRd32'); $cfg.shared = $true; $cfg.input = 'keys'
    }
}
if (-not $Resummarize) {
    if ($Exe) { $cfg.exe = $Exe }
    if ($PSBoundParameters.ContainsKey('AppArgs')) { $cfg.template = @($AppArgs) }
    if ($CaptureStdout) { $cfg.capture = $true }
    if ($InputMode -ne 'auto') { $cfg.input = $InputMode }
    if (-not $cfg.exe) { throw "No executable for preset '$Preset' (not installed?). Run with -Detect, or pass -Exe." }
    if (-not (Test-Path $cfg.exe)) { throw "Executable not found: $($cfg.exe)" }
    $ExePath = (Resolve-Path $cfg.exe).Path
    if ($cfg.shared -and -not $AllowSharedProfile) {
        throw "Preset '$Preset' has no isolated profile: it would use (and add recent-file entries to) your own profile. Re-run with -AllowSharedProfile if that is acceptable."
    }
    if (-not $Label) { $Label = if ($Preset -ne 'custom') { $Preset } else { [IO.Path]::GetFileNameWithoutExtension($ExePath) } }

    $PdfPath = $null; $PdfUri = $null
    if ($Pdf) {
        $cand = if ([IO.Path]::IsPathRooted($Pdf)) { $Pdf } else { Join-Path $Repo $Pdf }
        if (-not (Test-Path $cand)) { $cand = $Pdf }
        $PdfPath = (Resolve-Path $cand).Path
        $PdfUri = ([Uri]$PdfPath).AbsoluteUri
    }
    if ($cfg.open -eq 'cli' -and ($cfg.template -join ' ') -match '\{pdf(_uri)?\}' -and -not $PdfPath) {
        # launching without a document: drop the placeholder arguments
        $cfg.template = @($cfg.template | Where-Object { $_ -notmatch '\{pdf(_uri)?\}' })
    }
    if (-not $Scenario) {
        $Scenario = if ($PdfPath) { 'open-' + [IO.Path]::GetFileNameWithoutExtension($PdfPath) } else { 'launch-empty' }
    }
    if (-not $cfg.zoom -and $ZoomSteps -gt 0) { Write-Warning "Preset '$Preset' has no zoom feature; skipping zoom input."; $ZoomSteps = 0 }
    if ($cfg.singleInstance) {
        $running = Get-Process -Name $cfg.singleInstance -ErrorAction SilentlyContinue
        if ($running) { throw "$($cfg.singleInstance -join '/') is already running; a new launch would hand the file to that instance. Close it first." }
    }

    if (-not $OutFile) { $OutFile = Join-Path $Repo "benchmarks\runs\app-$Label.json" }
    if (-not $ShotsDir) { $ShotsDir = Join-Path $Repo 'benchmarks\runs\app-shots' }
    New-Item -ItemType Directory -Force (Split-Path -Parent $OutFile) | Out-Null
    if (-not $NoScreenshots) { New-Item -ItemType Directory -Force $ShotsDir | Out-Null }
    if (-not $ProfileRoot) { $ProfileRoot = Join-Path $env:TEMP 'fastpdf-bench-app' }

}

# ---------------------------------------------------------------- utilities
function Quote-Arg([string]$a) {
    if ($a -eq '') { return '""' }
    if ($a -notmatch '[\s"]') { return $a }
    $sb = New-Object System.Text.StringBuilder; [void]$sb.Append('"'); $bs = 0
    foreach ($ch in $a.ToCharArray()) {
        if ($ch -eq '\') { $bs++; continue }
        if ($ch -eq '"') { [void]$sb.Append('\' * (2 * $bs + 1)); [void]$sb.Append('"'); $bs = 0; continue }
        if ($bs) { [void]$sb.Append('\' * $bs); $bs = 0 }
        [void]$sb.Append($ch)
    }
    if ($bs) { [void]$sb.Append('\' * (2 * $bs)) }
    [void]$sb.Append('"'); return $sb.ToString()
}

function Expand-Args([string[]]$tmpl, [string]$profileDir) {
    foreach ($t in $tmpl) {
        $t.Replace('{pdf_uri}', [string]$PdfUri).Replace('{pdf}', [string]$PdfPath).Replace('{profile}', [string]$profileDir)
    }
}

function Use-LaunchBudget([string]$what) {
    if (-not $LaunchBudgetFile) { return }
    $state = if (Test-Path $LaunchBudgetFile) { Get-Content $LaunchBudgetFile -Raw | ConvertFrom-Json } else { [pscustomobject]@{ count = 0; log = @() } }
    if ($state.count -ge $MaxLaunches) { throw "Launch budget exhausted ($($state.count)/$MaxLaunches) - see $LaunchBudgetFile" }
    $state.count++
    $state.log = @($state.log) + @("$((Get-Date).ToString('s')) $what")
    $state | ConvertTo-Json -Depth 4 | Set-Content $LaunchBudgetFile -Encoding utf8
}

function New-ProfileDir([string]$suffix = '') {
    if ($cfg.profile -eq 'none') { return $null }
    $dir = Join-Path $ProfileRoot ("{0}-{1}-{2}{3}" -f $Preset, (Get-Date -Format 'yyyyMMdd-HHmmss'), (Get-Random -Maximum 99999), $suffix)
    New-Item -ItemType Directory -Force $dir | Out-Null
    switch ($cfg.profile) {
        'firefox' {
            @(
                'user_pref("browser.shell.checkDefaultBrowser", false);',
                'user_pref("browser.aboutwelcome.enabled", false);',
                'user_pref("browser.startup.homepage_override.mstone", "ignore");',
                'user_pref("startup.homepage_welcome_url", "");',
                'user_pref("startup.homepage_welcome_url.additional", "");',
                'user_pref("datareporting.policy.dataSubmissionEnabled", false);',
                'user_pref("datareporting.policy.firstRunURL", "");',
                'user_pref("datareporting.healthreport.uploadEnabled", false);',
                'user_pref("toolkit.telemetry.reportingpolicy.firstRun", false);',
                'user_pref("app.update.auto", false);',
                'user_pref("pdfjs.disabled", false);'
            ) | Set-Content (Join-Path $dir 'user.js') -Encoding ascii
        }
        'sumatra' { "CheckForUpdates = false`r`nRememberOpenedFiles = false`r`n" | Set-Content (Join-Path $dir 'SumatraPDF-settings.txt') -Encoding ascii }
    }
    return $dir
}

function Stop-ByCommandLine([string]$marker) {
    if (-not $marker) { return 0 }
    $n = 0
    foreach ($p in Get-CimInstance Win32_Process -ErrorAction SilentlyContinue) {
        if ($p.CommandLine -and $p.CommandLine.IndexOf($marker, [StringComparison]::OrdinalIgnoreCase) -ge 0 -and $p.ProcessId -ne $PID) {
            try { Stop-Process -Id $p.ProcessId -Force -ErrorAction Stop; $n++ } catch { }
        }
    }
    return $n
}

function Get-GpuMemory([int[]]$pids) {
    $paths = foreach ($id in $pids) { "\GPU Process Memory(pid_$($id)_*)\Dedicated Usage"; "\GPU Process Memory(pid_$($id)_*)\Shared Usage" }
    try {
        $c = Get-Counter -Counter $paths -ErrorAction SilentlyContinue
        if (-not $c) { return [ordered]@{ gpu_dedicated_mb = $null; gpu_shared_mb = $null } }
        $ded = ($c.CounterSamples | Where-Object Path -like '*dedicated usage' | Measure-Object CookedValue -Sum).Sum
        $sh = ($c.CounterSamples | Where-Object Path -like '*shared usage' | Measure-Object CookedValue -Sum).Sum
        return [ordered]@{ gpu_dedicated_mb = [math]::Round($ded / 1MB, 1); gpu_shared_mb = [math]::Round($sh / 1MB, 1) }
    } catch { return [ordered]@{ gpu_error = "$_" } }
}

function Convert-Sample($s) {
    [ordered]@{ processes = $s.Count; private_mb = [math]::Round($s.Private / 1MB, 1); ws_mb = [math]::Round($s.WorkingSet / 1MB, 1)
        threads = $s.Threads; handles = $s.Handles; cpu_ms_total = [math]::Round($s.Cpu100ns / 1e4, 1); names = $s.Names }
}

function Get-RecordOptions([int]$quiet, [int]$waitChange, [double]$notBefore) {
    $o = New-Object FastPdfBench.RecordOptions
    $o.StableFrames = $StableFrames; $o.QuietMs = $quiet; $o.TimeoutMs = $TimeoutSec * 1000
    $o.WaitChangeMs = $waitChange; $o.NotBeforeMs = $notBefore
    return $o
}

function Get-RelPath([string]$path) {
    try { return [IO.Path]::GetRelativePath($Repo, $path) } catch { }
    if ($path.StartsWith($Repo, [StringComparison]::OrdinalIgnoreCase)) { return $path.Substring($Repo.Length).TrimStart('') }
    return $path
}

function Save-Shot([byte[]]$px, [int]$w, [int]$h, [string]$name) {
    if ($NoScreenshots -or -not $px -or $w -le 0) { return $null }
    $file = Join-Path $ShotsDir ("{0}-{1}-{2}.png" -f $Label, $Scenario, $name)
    [FastPdfBench.Img]::SavePng($file, $px, $w, $h)
    return Get-RelPath $file
}

function Convert-Record($rec, [double]$t0) {
    $rel = { param($t) if ($t -lt 0) { $null } else { [math]::Round($t - $t0, 1) } }
    [ordered]@{
        t_first_nonblank_ms     = & $rel $rec.TFirstNonBlank
        t_first_change_ms       = & $rel $rec.TFirstChange
        t_visual_complete_ms    = & $rel $rec.TLastChange
        t_visual_complete_lower_ms = & $rel $rec.TBeforeLastChange
        t_stable_detected_ms    = & $rel $rec.TStable
        stable                  = $rec.Stable; timed_out = $rec.TimedOut; window_lost = $rec.WindowLost; no_effect = $rec.NoEffect
        frames                  = $rec.Frames.Count; changed_frames = $rec.ChangedFrames
        mean_capture_ms         = [math]::Round($rec.MeanCapMs, 1)
        client                  = "$($rec.W)x$($rec.H)"
        change_timeline         = $rec.ChangeTimeline($t0, 0.0005)
    }
}

function Invoke-Interaction($s, [string]$kind, [System.Diagnostics.Stopwatch]$clock) {
    $w = 0; $h = 0
    $px = $s.GrabFrame([ref]$w, [ref]$h)
    $base = [FastPdfBench.Img]::Signature($px, $w, $h, 4)
    $before = $s.Sample()
    $start = $clock.Elapsed.TotalMilliseconds + 60
    $cx = [int]($w / 2); $cy = [int]($h / 2)
    $useWheel = ($cfg.input -eq 'wheel') -or ($cfg.input -eq 'chromium' -and $kind -ne 'pagedown' -and $s.PointIsOurs($cx, $cy))
    $ctrl = $false; $method = ''
    switch ($kind) {
        'scroll' {
            if ($useWheel) { $steps = $s.WheelSteps($ScrollNotches, 50, -120, $false, $start); $method = "wheel x$ScrollNotches @50ms" }
            else { $steps = $s.KeySteps(0x28, $ScrollNotches, 50, $true, $start); $method = "Down x$ScrollNotches @50ms" }
        }
        'pagedown' { $steps = $s.KeySteps(0x22, $PageDowns, 150, $true, $start); $method = "PageDown x$PageDowns @150ms" }
        'zoom' {
            $ctrl = $true
            if ($useWheel) { $steps = $s.WheelSteps($ZoomSteps, 150, 120, $true, $start); $method = "Ctrl+wheel-up x$ZoomSteps @150ms" }
            else { $steps = $s.KeySteps(0xBB, $ZoomSteps, 200, $false, $start); $method = "Ctrl+'=' x$ZoomSteps @200ms" }
        }
    }
    $pb = $s.Play($steps, $ctrl, ($cfg.input -eq 'chromium'))
    $o = Get-RecordOptions 700 2500 ($pb.LastScheduledMs + 40)
    $rec = $s.Record($base, $o)
    $pb.Join()
    $after = $s.Sample()
    $sent = @($pb.SentAt)
    $firstIdx = 0
    for ($i = 0; $i -lt $steps.Count; $i++) { if ($steps[$i].Msg -ne [FastPdfBench.Native]::WM_MOUSEMOVE) { $firstIdx = $i; break } }
    $tFirst = if ($sent.Count -gt $firstIdx) { $sent[$firstIdx] } else { $null }
    $tLast = if ($sent.Count) { $sent[-1] } else { $null }
    $changeRate = $null
    if ($tFirst -and $tLast -and $tLast -gt $tFirst) {
        $inWindow = @($rec.Frames | Where-Object { $_.T -ge $tFirst -and $_.T -le ($tLast + 50) -and $_.Diff -gt 0.0005 }).Count
        $changeRate = [math]::Round($inWindow / (($tLast + 50 - $tFirst) / 1000), 1)
    }
    $r = [ordered]@{
        method                    = $method
        inputs_sent               = $sent.Count; inputs_skipped_unsafe = $pb.Skipped
        ctrl_injected             = $pb.CtrlInjected; playback_error = $pb.Error
        effect                    = -not $rec.NoEffect
        latency_first_change_ms   = if ($tFirst -and $rec.TFirstChange -ge 0) { [math]::Round($rec.TFirstChange - $tFirst, 1) } else { $null }
        settle_after_last_input_ms = if ($tLast -and $rec.TLastChange -ge 0) { [math]::Round([math]::Max(0, $rec.TLastChange - $tLast), 1) } else { $null }
        stable_after_last_input_ms = if ($tLast -and $rec.TStable -ge 0) { [math]::Round($rec.TStable - $tLast, 1) } else { $null }
        input_span_ms             = if ($tFirst -and $tLast) { [math]::Round($tLast - $tFirst, 1) } else { $null }
        changed_frames            = $rec.ChangedFrames
        changed_frames_per_s_during_input = $changeRate
        mean_capture_ms           = [math]::Round($rec.MeanCapMs, 1)
        cpu_ms                    = [math]::Round(($after.Cpu100ns - $before.Cpu100ns) / 1e4, 1)
        cpu_ms_net_of_capture     = if ($null -ne $script:OverheadPerS) { [math]::Round(($after.Cpu100ns - $before.Cpu100ns) / 1e4 - $script:OverheadPerS * (($after.T - $before.T) / 1000), 1) } else { $null }
        window_ms                 = [math]::Round($after.T - $before.T, 1)
        private_mb_after          = [math]::Round($after.Private / 1MB, 1)
        stable                    = $rec.Stable; timed_out = $rec.TimedOut
    }
    $r.screenshot = Save-Shot $rec.Final $rec.FinW $rec.FinH "r$($script:RunIndex)-after-$kind"
    return $r
}

# ---------------------------------------------------------------- one run
function Invoke-BenchRun([int]$index, [bool]$warmup, [string]$profileDir) {
    $script:RunIndex = $index
    $script:OverheadPerS = $null
    Use-LaunchBudget "$Label/$Scenario run $index$(if ($warmup) {' (warmup)'})"
    $run = [ordered]@{ index = $index; warmup = $warmup; started = (Get-Date).ToString('o'); cache_state = $CacheState }
    try { $run.cpu_load_pct_before = (Get-CimInstance Win32_Processor | Measure-Object -Property LoadPercentage -Average).Average } catch { }
    $argv = @(Expand-Args $cfg.template $profileDir)
    $psi = New-Object System.Diagnostics.ProcessStartInfo $ExePath
    $psi.Arguments = ($argv | ForEach-Object { Quote-Arg $_ }) -join ' '
    $psi.UseShellExecute = $false
    $psi.WorkingDirectory = Split-Path -Parent $ExePath
    # Always redirect + drain stdout/stderr (a GUI app must never block on a full pipe, and browser logs stay out of the console).
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.StandardOutputEncoding = [Text.Encoding]::UTF8
    if ($cfg.capture) { $psi.EnvironmentVariables['FASTPDF_BENCH'] = '1' }
    $clock = New-Object System.Diagnostics.Stopwatch
    $launchFt = [DateTime]::UtcNow.ToFileTimeUtc()
    $clock.Start()
    $p = [System.Diagnostics.Process]::Start($psi)
    $run.t_process_start_returned_ms = [math]::Round($clock.Elapsed.TotalMilliseconds, 1)
    $collector = New-Object FastPdfBench.StdoutCollector
    if ($cfg.capture) { $collector.Attach($p, $clock) } else { $collector.AttachDiscard($p) }
    $collector.AttachStderr($p)
    $s = New-Object FastPdfBench.Session -ArgumentList $p.Id, $clock, $launchFt
    # FASTPDF_BENCH t_ms is measured from process creation; host_ms ~= process_created_offset_ms + t_ms
    if ($s.Tracked[$p.Id] -gt 0) { $run.process_created_offset_ms = [math]::Round(($s.Tracked[$p.Id] - $launchFt) / 1e4, 2) }
    try {
        $tw = $s.WaitForWindow($TimeoutSec * 1000, 200, 150)
        if ($tw -lt 0) { throw "no main window within $TimeoutSec s (or the process exited)" }
        $run.t_window_ms = [math]::Round($tw, 1)
        $run.window = [ordered]@{ class = $s.Win.ClassName; title_len = $s.Win.Title.Length; client = "$($s.Win.ClientW)x$($s.Win.ClientH)"; dpi = $s.Win.Dpi }

        $rec = $s.Record($null, (Get-RecordOptions $QuietMs 0 0))
        $run.launch = Convert-Record $rec 0
        $run.launch.screenshot_first_nonblank = Save-Shot $rec.FirstNonBlank $rec.FnbW $rec.FnbH "r$index-first-nonblank"
        $run.launch.screenshot_final = Save-Shot $rec.Final $rec.FinW $rec.FinH "r$index-launch-final"
        $others = $s.ListOtherWindows()
        if ($others) { $run.other_windows = $others; throw "unexpected extra window(s) - possibly a consent/update/sign-in prompt; not touching it: $others" }

        if ($PdfPath -and $cfg.open -eq 'dialog') {
            $cr = $s.CenterRun()
            if ($cr[1] -lt 0) { throw "open button not found on the centre line" }
            $bw = 0; $bh = 0
            $basePx = $s.GrabFrame([ref]$bw, [ref]$bh)
            $baseSig = [FastPdfBench.Img]::Signature($basePx, $bw, $bh, 4)
            $s.ClickClient($cr[0], [int](($cr[1] + $cr[2]) / 2))
            $dlg = [IntPtr]::Zero; $sw = [Diagnostics.Stopwatch]::StartNew()
            while ($dlg -eq [IntPtr]::Zero -and $sw.ElapsedMilliseconds -lt 8000) { Start-Sleep -Milliseconds 20; $dlg = $s.FindTopWindowByTitle($cfg.dialogTitle) }
            if ($dlg -eq [IntPtr]::Zero) { throw "file dialog '$($cfg.dialogTitle)' did not appear" }
            Start-Sleep -Milliseconds 700
            $t0 = $clock.Elapsed.TotalMilliseconds
            $res = [FastPdfBench.Session]::OpenInCommonDialog($dlg, $PdfPath)
            if ($res -ne 'ok') { throw "dialog automation failed: $res" }
            $orec = $s.Record($baseSig, (Get-RecordOptions $QuietMs 8000 0))
            $run.open = Convert-Record $orec $t0
            $run.open.method = 'in-app file dialog (WM_SETTEXT + BM_CLICK); times relative to the Open click'
            $run.open.screenshot_final = Save-Shot $orec.Final $orec.FinW $orec.FinH "r$index-open-final"
        }

        Start-Sleep -Milliseconds 1000
        $a = $s.Sample()
        Start-Sleep -Seconds $IdleSeconds
        $b = $s.Sample()
        $run.idle = Convert-Sample $b
        $run.idle.seconds = $IdleSeconds
        $run.idle.cpu_ms = [math]::Round(($b.Cpu100ns - $a.Cpu100ns) / 1e4, 1)
        $run.idle.cpu_pct_of_one_core = [math]::Round((($b.Cpu100ns - $a.Cpu100ns) / 1e4) / ($IdleSeconds * 10), 2)
        $gm = Get-GpuMemory @($s.RefreshTree()); foreach ($k in $gm.Keys) { $run.idle[$k] = $gm[$k] }
        if ($b.Unreadable) { $run.idle.unreadable_pids = $b.Unreadable }

        # Observer-effect calibration: capture only (same cadence as the interaction recordings), no input.
        $cw = 0; $ch = 0
        $calPx = $s.GrabFrame([ref]$cw, [ref]$ch)
        $calSig = [FastPdfBench.Img]::Signature($calPx, $cw, $ch, 4)
        $k0 = $s.Sample()
        $cal = $s.Record($calSig, (Get-RecordOptions 700 2500 0))
        $k1 = $s.Sample()
        $calSec = [math]::Max(0.001, ($k1.T - $k0.T) / 1000)
        $run.capture_overhead = [ordered]@{ seconds = [math]::Round($calSec, 2); frames = $cal.Frames.Count; mean_capture_ms = [math]::Round($cal.MeanCapMs, 1)
            target_cpu_ms = [math]::Round(($k1.Cpu100ns - $k0.Cpu100ns) / 1e4, 1); target_cpu_ms_per_s = [math]::Round((($k1.Cpu100ns - $k0.Cpu100ns) / 1e4) / $calSec, 1)
            changed_while_idle = -not $cal.NoEffect; note = 'CPU the app spends while being captured with no input; subtract (per second) from interaction CPU' }
        $script:OverheadPerS = $run.capture_overhead.target_cpu_ms_per_s

        # Warm-up runs never interact: for profile-based apps their profile becomes the template that later runs copy,
        # and per-document view state (zoom / scroll position, e.g. Edge's PDF viewer) must not leak into it.
        $doInput = -not $warmup
        if ($doInput -and $ScrollNotches -gt 0) { $run.scroll = Invoke-Interaction $s 'scroll' $clock }
        if ($doInput -and $PageDowns -gt 0) { $run.pagedown = Invoke-Interaction $s 'pagedown' $clock }
        if ($doInput -and $ZoomSteps -gt 0) { $run.zoom = Invoke-Interaction $s 'zoom' $clock }
        if ($doInput -and ($ScrollNotches + $PageDowns + $ZoomSteps) -gt 0 -and $PostIdleSeconds -gt 0) {
            Start-Sleep -Milliseconds 500
            $c0 = $s.Sample(); Start-Sleep -Seconds $PostIdleSeconds; $c1 = $s.Sample()
            $run.post_interaction_idle = [ordered]@{ seconds = $PostIdleSeconds; cpu_ms = [math]::Round(($c1.Cpu100ns - $c0.Cpu100ns) / 1e4, 1); private_mb = [math]::Round($c1.Private / 1MB, 1); ws_mb = [math]::Round($c1.WorkingSet / 1MB, 1) }
        }
        $last = $s.Sample()
        $run.peak = [ordered]@{ private_mb_sum = [math]::Round($s.PeakPrivateSum / 1MB, 1); ws_mb_sum = [math]::Round($s.PeakWsSum / 1MB, 1)
            peak_ws_single_process_mb = [math]::Round($last.PeakWorkingSetMax / 1MB, 1); max_processes = $s.PeakProcessCount; note = 'sampled every ~250 ms while recording + at idle/interaction boundaries' }
    } catch {
        $run.error = "$_"
    } finally {
        $run.stdio = [ordered]@{ stdout_discarded_lines = $collector.DiscardedLines; stderr_lines = $collector.ErrLines; stderr_tail = @($collector.SnapshotErrTail()) }
        if ($cfg.capture) {
            $lines = $collector.SnapshotLines(); $times = $collector.SnapshotTimes()
            $events = @(); $byEvent = [ordered]@{}
            for ($i = 0; $i -lt $lines.Length; $i++) {
                $obj = $null; try { $obj = $lines[$i] | ConvertFrom-Json -ErrorAction Stop } catch { }
                if ($obj) {
                    $events += [ordered]@{ host_ms = [math]::Round($times[$i], 1); data = $obj }
                    if ($obj.event -and $null -ne $obj.t_ms) { $byEvent[[string]$obj.event] = $obj.t_ms }
                } else { $events += [ordered]@{ host_ms = [math]::Round($times[$i], 1); raw = $lines[$i] } }
            }
            $run.fastpdf_bench = [ordered]@{ lines = $lines.Length; events_t_ms = $byEvent; events = $events }
        }
        $graceful = $false
        if ($cfg.close -eq 'close') { try { $graceful = $s.CloseGracefully(8000) } catch { } }
        $killed = $s.KillTree()
        $byCmd = if ($profileDir) { Stop-ByCommandLine $profileDir } else { 0 }
        Start-Sleep -Milliseconds 300
        $run.shutdown = [ordered]@{ graceful = $graceful; killed_tracked = $killed; killed_by_profile_marker = $byCmd; alive_after = $s.AliveCount(); tracked = $s.TrackedNames() }
        $s.Dispose()
    }
    return $run
}

# ---------------------------------------------------------------- summary
function Get-NumericLeaves($obj, [string]$prefix) {
    $pairs = $null
    if ($obj -is [System.Collections.IDictionary]) { $pairs = foreach ($k in @($obj.Keys)) { ,@($k, $obj[$k]) } }
    elseif ($obj -is [System.Management.Automation.PSCustomObject]) { $pairs = foreach ($pp in $obj.PSObject.Properties) { ,@($pp.Name, $pp.Value) } }
    if ($null -ne $pairs) {
        # interaction blocks whose input had no visible effect are excluded from aggregation (kept in the raw runs)
        foreach ($kv in $pairs) { if ($kv[0] -eq 'effect' -and $kv[1] -eq $false) { return } }
        foreach ($kv in $pairs) { Get-NumericLeaves $kv[1] "$prefix$($kv[0])." }
    } elseif ($obj -is [double] -or $obj -is [int] -or $obj -is [long] -or $obj -is [single] -or $obj -is [decimal] -or $obj -is [uint32] -or $obj -is [int64]) {
        [pscustomobject]@{ k = $prefix.TrimEnd('.'); v = [double]$obj }
    }
}
function Get-Summary($runList) {
    $ok = @($runList | Where-Object { -not $_.warmup -and -not $_.error })
    $leaves = foreach ($r in $ok) { Get-NumericLeaves $r '' }
    $out = [ordered]@{ runs_ok = $ok.Count; runs_failed = @($runList | Where-Object { $_.error }).Count }
    foreach ($g in ($leaves | Where-Object { $_.k -notmatch '(^index$|cpu_load_pct_before|cpu_ms_total|handles$|\.dpi$)' } | Group-Object k)) {
        $v = @($g.Group.v | Sort-Object)
        $med = if ($v.Count % 2) { $v[[int][math]::Floor($v.Count / 2)] } else { ($v[$v.Count / 2 - 1] + $v[$v.Count / 2]) / 2 }
        $out[$g.Name] = [ordered]@{ median = [math]::Round($med, 1); min = [math]::Round($v[0], 1); max = [math]::Round($v[-1], 1); n = $v.Count }
    }
    return $out
}

function Get-MachineInfo {
    $m = [ordered]@{}
    try {
        $cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
        $cs = Get-CimInstance Win32_ComputerSystem; $os = Get-CimInstance Win32_OperatingSystem
        $m.cpu = $cpu.Name.Trim(); $m.cores = $cpu.NumberOfCores; $m.logical = $cpu.NumberOfLogicalProcessors
        $m.ram_gb = [math]::Round($cs.TotalPhysicalMemory / 1GB, 1)
        $m.os = "$($os.Caption) $($os.Version)"
        $m.gpus = @(Get-CimInstance Win32_VideoController | ForEach-Object { "$($_.Name) ($($_.DriverVersion))" })
        $m.displays = @(Get-CimInstance Win32_VideoController | Where-Object CurrentHorizontalResolution | ForEach-Object { "$($_.CurrentHorizontalResolution)x$($_.CurrentVerticalResolution)@$($_.CurrentRefreshRate)Hz" })
        $m.power_scheme = ((powercfg /getactivescheme) -join ' ').Trim()
    } catch { $m.error = "$_" }
    return $m
}

# ---------------------------------------------------------------- main
if ($script:ResummarizeOnly) {
    $docR = Get-Content $OutFile -Raw | ConvertFrom-Json
    foreach ($sc in $docR.scenarios) { $sc.summary = Get-Summary @($sc.runs) }
    $docR | ConvertTo-Json -Depth 14 | Set-Content $OutFile -Encoding utf8
    Write-Host "re-summarized $OutFile ($(@($docR.scenarios).Count) scenario(s))"
    return
}
$vi = (Get-Item $ExePath).VersionInfo
$appInfo = [ordered]@{
    label = $Label; preset = $Preset; exe = $ExePath; product_version = $vi.ProductVersion; file_version = $vi.FileVersion
    exe_bytes = (Get-Item $ExePath).Length; exe_mtime = (Get-Item $ExePath).LastWriteTime.ToString('o')
    args_template = @($cfg.template); profile_isolation = $cfg.profile; shared_user_profile = $cfg.shared; input_mode = $cfg.input
    open_method = if ($PdfPath) { $cfg.open } else { 'none' }; stdout_protocol = $cfg.capture
}
try { $gitDir = Split-Path -Parent $ExePath; $rev = (& git -C $gitDir rev-parse --short HEAD 2>$null); if ($LASTEXITCODE -eq 0 -and $rev) { $appInfo.git_rev = "$rev".Trim() } } catch { }

# Profiles: with -WarmupRuns >= 1 the warm-up run initialises a template profile and every measured run uses a fresh
# copy of it ("warm-copy": initialised profile, no per-document state from earlier runs). Without warm-up runs every
# run gets a brand-new empty profile ("fresh": includes first-run profile creation cost).
$templateDir = if ($WarmupRuns -gt 0) { New-ProfileDir '-template' } else { $null }
$profileDir = $null
# NOTE: not List[object]: pwsh 7.6 throws "Argument types do not match" for @() over List[object] holding OrderedDictionary
$results = New-Object System.Collections.ArrayList
try {
    $total = $WarmupRuns + $Runs
    for ($i = 1; $i -le $total; $i++) {
        $isWarm = $i -le $WarmupRuns
        $profileState = 'none'; $profileDir = $null
        if ($cfg.profile -ne 'none') {
            if ($isWarm) { $profileDir = $templateDir; $profileState = 'template-init' }
            elseif ($templateDir) {
                $profileDir = "$templateDir-run$i"
                Copy-Item -Recurse -Force $templateDir $profileDir
                $profileState = 'warm-copy'
            } else { $profileDir = New-ProfileDir "-run$i"; $profileState = 'fresh' }
        }
        Write-Host ("[{0}] {1}/{2} run {3}/{4}{5}" -f (Get-Date -Format 'HH:mm:ss'), $Label, $Scenario, $i, $total, $(if ($isWarm) { ' (warmup)' }))
        $r = Invoke-BenchRun $i $isWarm $profileDir
        $r.profile_state = $profileState
        [void]$results.Add($r)
        if ($profileDir -and $profileDir -ne $templateDir -and -not $KeepProfiles) {
            Start-Sleep -Milliseconds 500
            try { Remove-Item -Recurse -Force $profileDir -ErrorAction Stop } catch { Write-Warning "could not remove temp profile $profileDir : $_" }
        }
        $msg = if ($r.error) { "  error: $($r.error)" } else {
            "  window {0} ms, first non-blank {1} ms, visually complete {2} ms, idle private {3} MB, idle CPU {4} ms" -f $r.t_window_ms, $r.launch.t_first_nonblank_ms, $r.launch.t_visual_complete_ms, $r.idle.private_mb, $r.idle.cpu_ms
        }
        Write-Host $msg
        Start-Sleep -Seconds 2
    }
} finally {
    foreach ($d in @($templateDir, $profileDir)) {
        if ($d -and -not $KeepProfiles -and (Test-Path $d) -and $d.StartsWith($ProfileRoot, [StringComparison]::OrdinalIgnoreCase)) {
            Start-Sleep -Seconds 1
            try { Remove-Item -Recurse -Force $d -ErrorAction Stop } catch { Write-Warning "could not remove temp profile $d : $_" }
        }
    }
}

try {
    # keep the raw runs even if post-processing fails
    ConvertTo-Json -InputObject $results.ToArray() -Depth 14 | Set-Content ($OutFile + '.lastruns.tmp.json') -Encoding utf8
} catch { Write-Warning "raw dump failed: $($_.Exception.Message) @ $($_.InvocationInfo.PositionMessage)" }
try {
$scenarioObj = [ordered]@{
    name = $Scenario; generated = (Get-Date).ToString('o'); app = $appInfo
    pdf = if ($PdfPath) { [ordered]@{ path = (Get-RelPath $PdfPath); bytes = (Get-Item $PdfPath).Length } } else { $null }
    config = [ordered]@{ runs = $Runs; warmup_runs = $WarmupRuns; idle_seconds = $IdleSeconds; stable_frames = $StableFrames; quiet_ms = $QuietMs
        scroll_notches = $ScrollNotches; pagedowns = $PageDowns; zoom_steps = $ZoomSteps; cache_state = $CacheState; capture = 'PrintWindow(PW_CLIENTONLY|PW_RENDERFULLCONTENT), 4px grid' }
    summary = Get-Summary $results.ToArray()
    runs = $results.ToArray()
}
$doc = [ordered]@{ schema = $Schema; tool = "tools/bench-app/bench-app.ps1 $ToolVersion"; updated = (Get-Date).ToString('o'); machine = Get-MachineInfo; scenarios = @() }
if (Test-Path $OutFile) {
    try {
        $old = Get-Content $OutFile -Raw | ConvertFrom-Json
        if ($old.schema -eq $Schema) { $doc.scenarios = @($old.scenarios | Where-Object { $_.name -ne $Scenario }) }
    } catch { Write-Warning "existing $OutFile is not valid JSON; overwriting" }
}
$doc.scenarios = @($doc.scenarios) + @($scenarioObj)
$doc | ConvertTo-Json -Depth 14 | Set-Content $OutFile -Encoding utf8
Write-Host "wrote $OutFile"
$sum = $scenarioObj.summary
foreach ($k in 't_window_ms', 'launch.t_first_nonblank_ms', 'launch.t_visual_complete_ms', 'open.t_visual_complete_ms', 'idle.private_mb', 'idle.ws_mb', 'idle.cpu_ms', 'peak.private_mb_sum', 'scroll.latency_first_change_ms', 'scroll.settle_after_last_input_ms', 'zoom.latency_first_change_ms') {
    if ($sum.Contains($k)) { Write-Host ("  {0,-36} median {1,8}  [{2} .. {3}] n={4}" -f $k, $sum[$k].median, $sum[$k].min, $sum[$k].max, $sum[$k].n) }
}
Remove-Item ($OutFile + '.lastruns.tmp.json') -ErrorAction SilentlyContinue
} catch {
    Write-Host "post-processing failed: $($_.Exception.GetType().FullName): $($_.Exception.Message)"
    Write-Host $_.InvocationInfo.PositionMessage
    Write-Host $_.ScriptStackTrace
    throw
}
