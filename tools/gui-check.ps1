<#
.SYNOPSIS
  GUI check driver: runs FastPDF with a development script and captures the window at every mark= step.

.DESCRIPTION
  Drives a FastPDF build through FASTPDF_DEV_SCRIPT (crates/fastpdf-ui/src/devscript.rs):
    - waits until keyboard and mouse have been idle for -IdleSeconds and no cargo or rustc runs,
      so a launch neither disturbs the person at the machine nor races a build
    - launches -Exe with FASTPDF_SETTINGS_FILE and FASTPDF_RECENT_FILE set to non-empty files in
      -OutDir (never the profile in %APPDATA%\FastPDF), FASTPDF_DEV_OVERLAY=1 (dev scripts run in
      release builds only with it), FASTPDF_LOG=info, FASTPDF_PRINT_TO_FILE=-OutDir\print.out (print
      jobs never reach a printer; an earlier run's file is removed) and FASTPDF_DEV_SCRIPT=-Script;
      other FASTPDF_* variables of the calling shell are not passed on (use -AppEnv)
    - refuses a script that opens the signature panel (`sign-panel`, `cmd=Sign`) before `sign-test`:
      the panel would list the certificates of the Windows store
    - writes the app's log (stderr) to -OutDir\fastpdf.log
    - answers every `mark=PATH` step with a PrintWindow capture of the client area, PATH.png, and
      PATH.ack (PATH.mark, PATH.ack and PATH.png of an earlier run are removed before the launch)
    - closes the window a few seconds after the log shows the script ended (finished, failed, timed
      out or ignored), kills FastPDF and the processes it started after -TimeoutSeconds
    - compares names and modification times of the files in %APPDATA%\FastPDF before and after the
      run (never their contents) and prints PASS or FAIL
  Settings and the recent list stay in -OutDir between runs; a new -OutDir starts from defaults.
  Exit code 0 when the APPDATA check passed and no FastPDF process of the run is left.

.PARAMETER Exe
  The FastPDF executable, normally target\release\fastpdf.exe.
.PARAMETER Pdf
  The document to open; without it FastPDF shows its empty window.
.PARAMETER Script
  The FASTPDF_DEV_SCRIPT steps (the table at the top of crates/fastpdf-ui/src/devscript.rs).
.PARAMETER OutDir
  Settings, recent list, print output and log of the run; never inside %APPDATA%\FastPDF.
.PARAMETER IdleSeconds
  Keyboard and mouse idle time required before the launch.
.PARAMETER MaxIdleWaitSeconds
  Gives up (no launch) after waiting this long for idle input and no running build.
.PARAMETER TimeoutSeconds
  FastPDF and the processes it started are killed after running this long.
.PARAMETER Engine
  Passed as --engine NAME.
.PARAMETER AppEnv
  Extra environment for the app, NAME=value (NAME= removes it). FASTPDF_SETTINGS_FILE and
  FASTPDF_RECENT_FILE cannot be set; FASTPDF_PRINT_TO_FILE only to another file. pwsh -File passes
  one; several need pwsh -Command "& tools/gui-check.ps1 -AppEnv 'A=1','B=2' ...".

.EXAMPLE
  pwsh -NoProfile -File tools/gui-check.ps1 -Exe target\release\fastpdf.exe -Pdf fixtures\generated\large-text\dense-300p-times.pdf -OutDir $env:TEMP\gui\p12 -Script "wait-open; idle; page=12; expect-page=12; mark=$env:TEMP\gui\p12\page12; cmd=Quit"
.EXAMPLE
  pwsh -NoProfile -File tools/gui-check.ps1 -Exe target\release\fastpdf.exe -OutDir $env:TEMP\gui\empty -Engine hayro -AppEnv FASTPDF_LOG=debug -Script "wait=500; mark=$env:TEMP\gui\empty\window; cmd=Quit"
#>

#Requires -Version 7.4
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$Exe,
    [string]$Pdf,
    [Parameter(Mandatory)] [string]$Script,
    [Parameter(Mandatory)] [string]$OutDir,
    [int]$IdleSeconds = 20,
    [int]$MaxIdleWaitSeconds = 180,
    [int]$TimeoutSeconds = 120,
    [string]$Engine,
    [string[]]$AppEnv
)

$ErrorActionPreference = 'Stop'

# Variables that keep the run away from the owner's profile: never taken from -AppEnv.
$IsolationVariables = @('FASTPDF_SETTINGS_FILE', 'FASTPDF_RECENT_FILE')
# Seconds between the end of the script in the log and closing the window.
$CloseGraceSeconds = 2

if (-not ('FastPdfGuiCheck.Shot' -as [type])) {
    # .NET 10 moved Bitmap's base types into System.Private.Windows.*.
    $references = @('System.Drawing.Common', 'System.Drawing.Primitives')
    Add-Type -AssemblyName System.Drawing.Common
    $runtime = Split-Path ([System.Drawing.Bitmap].Assembly.Location)
    foreach ($name in 'System.Private.Windows.Core.dll', 'System.Private.Windows.GdiPlus.dll') {
        $path = Join-Path $runtime $name
        if (Test-Path $path) { $references += $path }
    }
    Add-Type -ReferencedAssemblies $references -TypeDefinition @'
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
namespace FastPdfGuiCheck {
    public static class Shot {
        [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
        [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr h, out RECT r);
        [DllImport("user32.dll")] static extern bool GetLastInputInfo(ref LASTINPUTINFO l);
        [DllImport("user32.dll")] static extern bool IsIconic(IntPtr h);
        [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr h, int cmd);
        [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
        [StructLayout(LayoutKind.Sequential)] public struct LASTINPUTINFO { public uint cbSize; public uint dwTime; }
        const uint PW_CLIENTONLY_RENDERFULLCONTENT = 3;
        const int SW_SHOWNOACTIVATE = 4;

        // A minimized window captures black: show it again without taking the focus.
        public static bool Unminimize(IntPtr hwnd) {
            if (!IsIconic(hwnd)) return false;
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            return true;
        }

        public static bool Save(IntPtr hwnd, string path) {
            RECT r;
            if (!GetClientRect(hwnd, out r) || r.R - r.L <= 0 || r.B - r.T <= 0) return false;
            using (var bmp = new Bitmap(r.R - r.L, r.B - r.T, PixelFormat.Format32bppArgb))
            using (var g = Graphics.FromImage(bmp)) {
                IntPtr hdc = g.GetHdc();
                bool ok = PrintWindow(hwnd, hdc, PW_CLIENTONLY_RENDERFULLCONTENT);
                g.ReleaseHdc(hdc);
                if (ok) bmp.Save(path, ImageFormat.Png);
                return ok;
            }
        }

        // Tick count of the last keyboard or mouse input of the session.
        public static uint LastInput() {
            var l = new LASTINPUTINFO();
            l.cbSize = (uint)Marshal.SizeOf(typeof(LASTINPUTINFO));
            GetLastInputInfo(ref l);
            return l.dwTime;
        }

        public static uint IdleMs() {
            return (uint)Environment.TickCount - LastInput();
        }
    }
}
'@
}
$Shot = [FastPdfGuiCheck.Shot]

function Get-AppDataTimes([string]$Dir) {
    # Relative name -> modification time of every file in the profile; contents are never read.
    $times = @{}
    if (Test-Path -LiteralPath $Dir) {
        foreach ($file in Get-ChildItem -LiteralPath $Dir -File -Recurse -Force) {
            $times[$file.FullName.Substring($Dir.Length).TrimStart('\')] = $file.LastWriteTimeUtc.Ticks
        }
    }
    $times
}

function Get-ChangedNames([hashtable]$Before, [hashtable]$After) {
    @(@($Before.Keys) + @($After.Keys) | Select-Object -Unique |
        Where-Object { $Before[$_] -ne $After[$_] } | Sort-Object)
}

function Get-Steps([string]$Steps) {
    # Name and value of every step, split like devscript::parse does.
    foreach ($step in $Steps -split ';') {
        $step = $step.Trim()
        if (-not $step) { continue }
        $eq = $step.IndexOf('=')
        if ($eq -lt 0) {
            [pscustomobject]@{ Name = $step; Value = $null }
        } else {
            [pscustomobject]@{ Name = $step.Substring(0, $eq).Trim(); Value = $step.Substring($eq + 1).Trim() }
        }
    }
}

function Get-MarkPaths([string]$Steps, [string]$Base) {
    # The PATH of every `mark=PATH` step.
    foreach ($step in Get-Steps $Steps) {
        if ($step.Name -ceq 'mark' -and $step.Value) { [IO.Path]::GetFullPath($step.Value, $Base) }
    }
}

function Test-SignPanelFirst([string]$Steps) {
    # True when the signature panel opens before `sign-test`: it would list the store's certificates.
    # Case-insensitive like command_named; a step devscript rejects only makes this stricter.
    foreach ($step in Get-Steps $Steps) {
        if ($step.Name -eq 'sign-test') { return $false }
        if ($step.Name -eq 'sign-panel' -or ($step.Name -eq 'cmd' -and $step.Value -eq 'Sign')) { return $true }
    }
    $false
}

function Get-Tree([int]$RootId) {
    # Process ids of $RootId's descendants (render hosts, writers).
    $all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId
    $ids = @($RootId)
    $found = @()
    while ($ids.Count -gt 0) {
        $children = @($all | Where-Object { $ids -contains $_.ParentProcessId } | ForEach-Object { [int]$_.ProcessId })
        $found += $children
        $ids = $children
    }
    $found
}

function Get-RunProcess([int[]]$Ids, [datetime]$Launched) {
    # The FastPDF processes among $Ids that this run started (a reused id belongs to someone else).
    foreach ($id in $Ids) {
        $process = Get-Process -Id $id -ErrorAction SilentlyContinue
        if ($process -and $process.ProcessName -like 'fastpdf*' -and $process.StartTime -ge $Launched) { $process }
    }
}

function Read-Log([string]$Path) {
    # The log so far; FastPDF still holds the file open for writing.
    try {
        $stream = [IO.File]::Open($Path, 'Open', 'Read', 'ReadWrite, Delete')
        try { (New-Object IO.StreamReader($stream)).ReadToEnd() } finally { $stream.Dispose() }
    } catch {
        ''
    }
}

function Get-ScriptEnd([string]$Log) {
    # The log line that ended the dev script, from `dev script` on; $null while it runs. Matched on
    # the level and the start of the message (`{secs} {LEVEL} {target}: {message}`, logger.rs):
    # step text such as find=TEXT can contain any words. The only WARN lines saying `dev script: step`
    # are the step that failed or timed out.
    $end = [regex]::Match($Log,
        '(?m)^\s*[\d.]+ (?:WARN\s+\S+: (dev script(?: ignored: |: step \d+ ).*)|INFO\s+\S+: (dev script: finished))\r?$')
    if (-not $end.Success) { return $null }
    $line = if ($end.Groups[1].Success) { $end.Groups[1].Value } else { $end.Groups[2].Value }
    $line.TrimEnd()
}

# ---------------------------------------------------------------- checks before the launch
$cwd = (Get-Location -PSProvider FileSystem).ProviderPath
$exePath = (Resolve-Path -LiteralPath $Exe).ProviderPath
$pdfPath = if ([string]::IsNullOrWhiteSpace($Pdf)) { $null } else { (Resolve-Path -LiteralPath $Pdf).ProviderPath }
if ($Engine -and $Engine -notmatch '^[A-Za-z0-9_-]+$') { throw "-Engine '$Engine' is not an engine name" }
if (Test-SignPanelFirst $Script) { throw 'sign panel before sign-test; not launching' }
if ([string]::IsNullOrWhiteSpace($OutDir)) { throw '-OutDir is empty' }
$appDataDir = [IO.Path]::GetFullPath((Join-Path $env:APPDATA 'FastPDF'))
$outPath = [IO.Path]::GetFullPath($OutDir, $cwd)
if (($outPath.TrimEnd('\') + '\').StartsWith($appDataDir.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) {
    throw "-OutDir is inside $appDataDir; not launching"
}
New-Item -ItemType Directory -Force $outPath | Out-Null

$appEnvironment = @{}
foreach ($pair in @($AppEnv)) {
    if (-not $pair) { continue }
    $eq = $pair.IndexOf('=')
    if ($eq -lt 1) { throw "-AppEnv '$pair' is not NAME=value" }
    $name = $pair.Substring(0, $eq)
    if ($IsolationVariables -contains $name) { throw "-AppEnv may not set $name (the run keeps it in -OutDir)" }
    $value = $pair.Substring($eq + 1)
    if ($name -eq 'FASTPDF_PRINT_TO_FILE' -and -not $value) {
        throw '-AppEnv may not remove FASTPDF_PRINT_TO_FILE (print jobs would reach a printer)'
    }
    $appEnvironment[$name] = if ($value) { $value } else { $null }
}

$settingsFile = Join-Path $outPath 'settings.toml'
$recentFile = Join-Path $outPath 'recent.txt'
if ([string]::IsNullOrWhiteSpace($settingsFile) -or [string]::IsNullOrWhiteSpace($recentFile)) {
    throw 'isolation paths are empty; not launching'
}
# Start-Process -Environment: $null removes a variable from the app's environment.
$environment = @{}
foreach ($name in [Environment]::GetEnvironmentVariables().Keys) {
    if ($name -like 'FASTPDF_*') { $environment[$name] = $null }
}
$environment['FASTPDF_LOG'] = 'info'
$printFile = Join-Path $outPath 'print.out'
$environment['FASTPDF_PRINT_TO_FILE'] = $printFile
foreach ($name in $appEnvironment.Keys) { $environment[$name] = $appEnvironment[$name] }
$environment['FASTPDF_SETTINGS_FILE'] = $settingsFile
$environment['FASTPDF_RECENT_FILE'] = $recentFile
$environment['FASTPDF_DEV_OVERLAY'] = '1'
$environment['FASTPDF_DEV_SCRIPT'] = $Script

$marks = @(Get-MarkPaths $Script $cwd)
foreach ($twice in $marks | Group-Object | Where-Object Count -gt 1) {
    Write-Output "WARN mark $($twice.Name) appears $($twice.Count) times; only the first is captured"
}
$marks = @($marks | Select-Object -Unique)
foreach ($mark in $marks) {
    New-Item -ItemType Directory -Force (Split-Path -Parent $mark) | Out-Null
    Remove-Item -LiteralPath "$mark.mark", "$mark.ack", "$mark.png" -Force -ErrorAction SilentlyContinue
}
Remove-Item -LiteralPath $printFile -Force -ErrorAction SilentlyContinue

$waited = 0
while ($waited -lt $MaxIdleWaitSeconds) {
    $builds = @(Get-Process -Name cargo, rustc -ErrorAction SilentlyContinue)
    if ($Shot::IdleMs() -ge $IdleSeconds * 1000 -and $builds.Count -eq 0) { break }
    Start-Sleep -Seconds 1
    $waited++
}
$builds = @(Get-Process -Name cargo, rustc -ErrorAction SilentlyContinue)
Write-Output "input idle: $($Shot::IdleMs()) ms, builds running: $($builds.Count), after waiting $waited s"
if ($Shot::IdleMs() -lt $IdleSeconds * 1000) { throw 'keyboard or mouse in use; not launching' }
if ($builds.Count -gt 0) { throw 'cargo or rustc is running; not launching' }

# ---------------------------------------------------------------- the run
$appDataBefore = Get-AppDataTimes $appDataDir
$arguments = @()
if ($Engine) { $arguments += '--engine', $Engine }
if ($pdfPath) { $arguments += "`"$pdfPath`"" }
$log = Join-Path $outPath 'fastpdf.log'
$launch = @{
    FilePath = $exePath
    WorkingDirectory = $cwd
    Environment = $environment
    RedirectStandardError = $log
    PassThru = $true
}
if ($arguments.Count -gt 0) { $launch.ArgumentList = $arguments -join ' ' }
$inputBefore = $Shot::LastInput()
$launched = (Get-Date).AddSeconds(-1)
$proc = Start-Process @launch
Write-Output "launched pid $($proc.Id): $(Split-Path -Leaf $exePath) $($arguments -join ' ')"
$deadline = (Get-Date).AddSeconds($TimeoutSeconds)
$done = @{}
$tree = @()
$scriptEnd = $null
$endSeen = $null
$ticks = 0
try {
    while (-not $proc.HasExited -and (Get-Date) -lt $deadline) {
        if ($ticks % 20 -eq 0) {
            $tree = @(@($tree) + @(Get-Tree $proc.Id) | Where-Object { $_ } | Select-Object -Unique)
        }
        foreach ($mark in $marks) {
            if ($done.ContainsKey($mark) -or -not (Test-Path -LiteralPath "$mark.mark")) { continue }
            $proc.Refresh()
            if ($Shot::Unminimize($proc.MainWindowHandle)) { Start-Sleep -Milliseconds 300 }
            $ok = $Shot::Save($proc.MainWindowHandle, "$mark.png")
            Write-Output "mark $(Split-Path -Leaf $mark): captured=$ok"
            Set-Content -LiteralPath "$mark.ack" -Value 'ok'
            $done[$mark] = $ok
        }
        if ($ticks % 5 -eq 0 -and -not $scriptEnd) {
            $scriptEnd = Get-ScriptEnd (Read-Log $log)
            if ($scriptEnd) { $endSeen = Get-Date }
        }
        if ($endSeen -and (Get-Date) -gt $endSeen.AddSeconds($CloseGraceSeconds)) {
            Write-Output 'script ended: closing the window'
            $proc.Refresh()
            $proc.CloseMainWindow() | Out-Null
            if (-not $proc.WaitForExit(10000)) { Write-Output 'window did not close' }
            break
        }
        $ticks++
        Start-Sleep -Milliseconds 100
    }
} finally {
    $tree = @(@($tree) + @(Get-Tree $proc.Id) | Where-Object { $_ } | Select-Object -Unique)
    if (-not $proc.HasExited) {
        Write-Output "stopping FastPDF$(if ((Get-Date) -ge $deadline) { ' (timeout)' })"
        Stop-Process -Id $proc.Id -Force
    }
    Start-Sleep -Milliseconds 500
    foreach ($child in Get-RunProcess $tree $launched) {
        Write-Output "stopping child $($child.Id) $($child.ProcessName)"
        Stop-Process -Id $child.Id -Force
    }
}
$proc.WaitForExit()

# ---------------------------------------------------------------- report
Write-Output "exit code: $($proc.ExitCode)"
if (-not $scriptEnd) { $scriptEnd = Get-ScriptEnd (Read-Log $log) }
Write-Output "script: $(if ($scriptEnd) { $scriptEnd } else { 'no end in the log' })"
Write-Output "marks captured: $(@($done.Values | Where-Object { $_ }).Count) of $($marks.Count)"
if ($Shot::LastInput() -ne $inputBefore) { Write-Output 'WARN keyboard or mouse input during the run' }
Start-Sleep -Milliseconds 500
$left = @(Get-RunProcess (@($proc.Id) + $tree) $launched)
if ($left.Count -gt 0) {
    Write-Output "left over: FAIL ($(($left | ForEach-Object Id) -join ', '))"
} else {
    Write-Output 'left over: PASS (no FastPDF process of this run)'
}
$changed = @(Get-ChangedNames $appDataBefore (Get-AppDataTimes $appDataDir))
if ($changed.Count -gt 0) {
    Write-Output "APPDATA check: FAIL (changed: $($changed -join ', '))"
} else {
    Write-Output 'APPDATA check: PASS (mtimes unchanged)'
}
exit $(if ($changed.Count -eq 0 -and $left.Count -eq 0) { 0 } else { 1 })
