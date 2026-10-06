#Requires -Version 5.1
# S1c measurement on the PC (docs/superpowers/specs/2026-09-27-s1c-windows-tuning-design.md section 4.1):
# the xperf kernel trace with the spike's glitch markers, the dpcisr and
# dumper reports, per-CPU counter samples over WMI (language-neutral), the
# System log, the WPT install, and the REAPER-mode fingerprint and inventory.
# Changes no Windows setting.
#
# -ArgumentList 'stop-only' loads only what the pre-emption stop needs
# (Stop-IemTraceSessions): IemTuning is not loaded at all (review R2).
param([string]$Load = 'all')
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (@('all', 'stop-only') -notcontains $Load) { throw "IemMeasure: -ArgumentList '$Load': use 'all' or 'stop-only'" }
# IemTuning (Invoke-IemNative, the profile, the journal) serves every function
# here but the pre-emption stop. Nothing is compiled at this import, and a
# failure to load IemTuning (its Add-Type compiles) is kept, never thrown: this
# module, and so the stop, loads regardless (review R2). A function that needs
# IemTuning then fails when called.
$script:TuningLoadError = $null
if ($Load -eq 'all') {
    try { Import-Module (Join-Path $PSScriptRoot 'IemTuning.psm1') -Force -Global }
    catch { $script:TuningLoadError = "$_" }
}
# crates/iem-audio-io/src/os.rs MARKER_PROVIDER.
$script:MarkerProvider = '3b6c1e0a-5d2f-4c8e-9a71-0e4f2d9b8c11'
$script:MarkerSession = 'IemMarkers'
$script:KernelSession = 'NT Kernel Logger'
# The pre-emption stop's logman: a Windows binary by its full path (no signature
# check needed, unlike xperf); the self-test points it at a stand-in.
$script:Logman = Join-Path $env:SystemRoot 'System32\logman.exe'
# logman's exit code for a session that does not run (PLA_E_DCS_NOT_FOUND, 0x80300002):
# one that ended between two calls is gone, never an error (CI run 37464797322).
$script:SessionNotFound = -2144337918
$script:NearEvents = @('DPC', 'TimedDPC', 'ThreadedDPC', 'Interrupt', 'CSwitch', 'ReadyThread')
# xperf: WPT 10 or newer (the toolkit the dpcisr/dumper parsers read), Microsoft-signed.
$script:XperfMinVersion = [version]'10.0'
$script:XperfChecked = @{}   # paths verified in this process (m4)
# The timer resolution the inventory records (design note 6.6); compiled only
# when the inventory runs, not at every import.
$script:TimerSource = @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;

public static class IemTimer {
    [DllImport("ntdll.dll")] static extern int NtQueryTimerResolution(out uint coarsest, out uint finest, out uint current);
    // 100 ns units: coarsest, finest, current.
    public static uint[] Query() {
        uint a, b, c;
        int rc = NtQueryTimerResolution(out a, out b, out c);
        if (rc != 0) throw new Win32Exception(rc);
        return new uint[] { a, b, c };
    }
}
'@

function Get-IemNow { (Get-Date).ToUniversalTime().ToString('o') }

function New-IemTraceArguments {
    param([Parameter(Mandatory)][string]$Dir, [switch]$CSwitch, [int]$CircularMB = 0)
    $flags = 'PROC_THREAD+LOADER+DPC+INTERRUPT'
    if ($CSwitch) { $flags += '+CSWITCH+DISPATCHER' }
    $a = @('-on', $flags, '-BufferSize', '1024', '-MinBuffers', '256', '-MaxBuffers', '1024')
    if ($CircularMB -gt 0) { $a += @('-FileMode', 'Circular', '-MaxFile', "$CircularMB") }
    $a += @('-f', (Join-Path $Dir 'kernel.etl'), '-start', $script:MarkerSession, '-on', $script:MarkerProvider, '-f', (Join-Path $Dir 'markers.etl'))
    return ,$a
}

function Invoke-XperfRun {
    # Module-private (not exported): runs xperf. -Verify: it runs elevated, so only a
    # Microsoft-signed, new-enough binary, checked once per process (m4). stderr is
    # output, the exit code alone decides (A11).
    param([Parameter(Mandatory)][string]$Xperf, [Parameter(Mandatory)][string[]]$Arguments, [switch]$Verify)
    if (-not (Test-Path -LiteralPath $Xperf)) { throw "xperf not found at $Xperf (run wpt-install)" }
    if ($Verify -and -not $script:XperfChecked.ContainsKey($Xperf)) {
        [void](Assert-IemXperf -Xperf $Xperf -MinVersion $script:XperfMinVersion)
        $script:XperfChecked[$Xperf] = $true
    }
    $r = Invoke-IemNative -FilePath $Xperf -Arguments $Arguments
    if ($r.code -ne 0) { throw "xperf $($Arguments -join ' ') (exit $($r.code)): $($r.out -join ' ')" }
    return ,@($r.out)
}

function Invoke-IemXperf {
    param([Parameter(Mandatory)][string]$Xperf, [Parameter(Mandatory)][string[]]$Arguments)
    $out = Invoke-XperfRun -Xperf $Xperf -Arguments $Arguments -Verify
    return ,$out
}

function Start-IemTrace {
    # The run folder in the stop's canonical form (ConvertTo-TraceDir), so the
    # kernel logger's output file is found under it again (#32 MAJOR-1).
    param([Parameter(Mandatory)][string]$Xperf, [Parameter(Mandatory)][string]$Dir, [switch]$CSwitch, [int]$CircularMB = 0)
    $Dir = ConvertTo-TraceDir -Dir $Dir
    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    [void](Invoke-IemXperf -Xperf $Xperf -Arguments (New-IemTraceArguments -Dir $Dir -CSwitch:$CSwitch -CircularMB $CircularMB))
    [pscustomobject]@{ dir = $Dir; started = Get-IemNow }
}

function ConvertTo-TraceDir {
    # Module-private (#32 MAJOR-1): a trace's run folder in one canonical form, so
    # xperf's -f (the start) and every stop use one text: GetFullPath folds doubled
    # and forward separators and . or .. segments. Never rely on ETW folding a path:
    # the start must go through here too. It is a folder on a drive, never a drive
    # root or a relative path (under which every kernel trace would count as ours),
    # checked before and after.
    param([Parameter(Mandatory)][string]$Dir)
    $refused = "trace directory '$Dir': not a folder on a drive (X:\folder); a drive root or a relative path is refused"
    if ($Dir -notmatch '^[A-Za-z]:[\\/]') { throw $refused }
    $full = [IO.Path]::GetFullPath($Dir).TrimEnd('\')
    if ($full -notmatch '^[A-Za-z]:\\[^\\]+') { throw $refused }
    return $full
}

function Test-OwnTraceOutput {
    # Module-private (#32 MAJOR-1): whether logman's description of one session
    # names an output file under Dir. Any line holding Dir and a path separator
    # counts, case-insensitive: logman's field labels are localized, so none is
    # keyed on.
    param([AllowEmptyCollection()][string[]]$Lines = @(), [Parameter(Mandatory)][string]$Dir)
    $prefix = $Dir.TrimEnd('\') + '\'
    foreach ($l in @($Lines)) { if ($l.IndexOf($prefix, [StringComparison]::OrdinalIgnoreCase) -ge 0) { return $true } }
    return $false
}

function Get-TraceOwnership {
    # Module-private (#32 MAJOR-1): which of our sessions run and are ours to stop
    # ('own', the kernel logger first), which run and are not ('kept'), and what
    # could not be read ('errors'). Ownership is a property of the session itself:
    # the NT Kernel Logger is ours exactly when its output file lies under Dir, the
    # trace's run folder (logman query "NT Kernel Logger" -ets), never another
    # tool's (LatencyMon, ProcMon); IemMarkers is ours by its name. Whether
    # IemMarkers runs proves nothing about the kernel logger: xperf -on may start
    # only the kernel logger, and a partial stop may leave it alone. When the list
    # of sessions cannot be read, both are looked at.
    param([Parameter(Mandatory)][string]$Dir, [Parameter(Mandatory)][int]$TimeoutSeconds)
    $Dir = ConvertTo-TraceDir -Dir $Dir
    $o = [pscustomobject]@{ own = @(); kept = @(); errors = @() }
    $listed = $null
    $q = Invoke-LogmanRun -Arguments @('query', '-ets') -TimeoutSeconds $TimeoutSeconds
    if ($q.ok) {
        $listed = @(foreach ($s in $script:KernelSession, $script:MarkerSession) {
            if (@(@($q.out) | Where-Object { $_ -match ('^\s*' + [regex]::Escape($s) + '\s') }).Count -gt 0) { $s }
        })
    } else { $o.errors += $q.error }
    if ($null -eq $listed -or $listed -contains $script:KernelSession) {
        $k = Invoke-LogmanRun -Arguments @('query', $script:KernelSession, '-ets') -TimeoutSeconds $TimeoutSeconds
        if (-not $k.ok -and $k.code -eq $script:SessionNotFound) { }   # ended since the list: nothing to stop
        elseif (-not $k.ok) { $o.errors += "$($script:KernelSession): whose trace it is cannot be read, not stopped ($($k.error))" }
        elseif (Test-OwnTraceOutput -Lines $k.out -Dir $Dir) { $o.own += $script:KernelSession }
        else { $o.kept += $script:KernelSession }
    }
    if ($null -eq $listed -or $listed -contains $script:MarkerSession) { $o.own += $script:MarkerSession }
    return $o
}

function Get-KeptTraceText {
    # Module-private: the error a kept session is (#32 MAJOR-1).
    param([AllowEmptyCollection()][string[]]$Kept = @())
    return @(foreach ($s in @($Kept)) { "$s runs, but its output file is not under the trace directory: another tool's trace, not stopped" })
}

function Invoke-LogmanRun {
    # Module-private: one logman.exe call, bounded. Needs nothing from IemTuning.
    # A call that does not finish in time is reported and left to finish on its
    # own: nothing is force-ended (I8).
    param([Parameter(Mandatory)][string[]]$Arguments, [Parameter(Mandatory)][int]$TimeoutSeconds)
    $si = New-Object System.Diagnostics.ProcessStartInfo
    $si.FileName = $script:Logman
    $si.Arguments = (@($Arguments | ForEach-Object { if ($_ -match '\s') { '"' + $_ + '"' } else { $_ } }) -join ' ')
    $si.UseShellExecute = $false
    $si.CreateNoWindow = $true
    $si.RedirectStandardOutput = $true
    $si.RedirectStandardError = $true
    $what = "logman $($si.Arguments)"
    try { $p = [System.Diagnostics.Process]::Start($si) }
    catch { return [pscustomobject]@{ ok = $false; out = @(); error = "${what}: $($_.Exception.GetBaseException().Message)"; code = $null } }
    $out = $p.StandardOutput.ReadToEndAsync()
    $err = $p.StandardError.ReadToEndAsync()
    if (-not $p.WaitForExit($TimeoutSeconds * 1000)) { return [pscustomobject]@{ ok = $false; out = @(); error = "$what did not finish within $TimeoutSeconds s"; code = $null } }
    # The output is what tells which sessions run and where they write: output not
    # read within 5 s of the exit (a process it started keeps the pipe open) is an
    # error, never an empty "nothing runs" (#32 MINOR-2).
    if (-not [System.Threading.Tasks.Task]::WaitAll([System.Threading.Tasks.Task[]]@($out, $err), 5000)) {
        return [pscustomobject]@{ ok = $false; out = @(); error = "$what exited, but its output was not read within 5 s"; code = $null }
    }
    $text = @(($out.Result + "`n" + $err.Result) -split "`r?`n" | Where-Object { $_.Trim() -ne '' })
    if ($p.ExitCode -ne 0) { return [pscustomobject]@{ ok = $false; out = $text; error = "$what (exit $($p.ExitCode)): $($text -join ' ')"; code = $p.ExitCode } }
    return [pscustomobject]@{ ok = $true; out = $text; error = $null; code = 0 }
}

function Stop-IemTraceSessions {
    # The trace stop every caller uses: the pre-emption at "ide event", trace-stop,
    # a failed measure's cleanup, a cut, the final stop (review 3.6, R2; #32 MAJOR-1).
    # It needs neither xperf nor IemTuning, so it works whatever else fails to load
    # (-ArgumentList 'stop-only' loads nothing else), and it is idempotent: nothing
    # running is success. -Dir is the trace's run folder, which decides ownership
    # (Get-TraceOwnership): the NT Kernel Logger is ours exactly when its output
    # file lies there. logman.exe stops the kernel logger first and IemMarkers
    # after it, each attempted on its own, each call bounded. Every error, output
    # that was not read, and a kernel logger that runs but is not ours (kept) fail
    # the call at the end, naming them all and what did stop: the caller keeps the
    # trace recorded and alarms, never reads it as stopped.
    param([Parameter(Mandatory)][string]$Dir, [ValidateRange(1, 600)][int]$TimeoutSeconds = 30)
    $o = Get-TraceOwnership -Dir $Dir -TimeoutSeconds $TimeoutSeconds
    $errors = @($o.errors); $stopped = @(); $gone = @()
    foreach ($s in $script:KernelSession, $script:MarkerSession) {
        if (@($o.own) -notcontains $s) { continue }
        $r = Invoke-LogmanRun -Arguments @('stop', $s, '-ets') -TimeoutSeconds $TimeoutSeconds
        if ($r.ok) { $stopped += $s }
        elseif ($r.code -eq $script:SessionNotFound) { $gone += $s }   # ended by itself since the query
        else { $errors += $r.error }
    }
    $errors += @(Get-KeptTraceText -Kept $o.kept)
    if ($errors.Count -gt 0) {
        $done = 'none'
        if ($stopped.Count -gt 0) { $done = $stopped -join ', ' }
        throw ("trace stop: $($errors -join '; ') (stopped: $done)")
    }
    return [pscustomobject]@{ stopped = @($stopped); gone = @($gone); kept = @($o.kept); via = 'logman'; tuning_error = $script:TuningLoadError }
}

function Invoke-IemDpcIsr {
    param([Parameter(Mandatory)][string]$Xperf, [Parameter(Mandatory)][string]$Dir, [string]$Name = 'trace.etl')
    $out = Join-Path $Dir $(if ($Name -eq 'trace.etl') { 'dpcisr.txt' } else { [IO.Path]::GetFileNameWithoutExtension($Name) + '.dpcisr.txt' })
    [void](Invoke-IemXperf -Xperf $Xperf -Arguments @('-i', (Join-Path $Dir $Name), '-o', $out, '-a', 'dpcisr'))
    return $out
}

function Get-IemNearGlitchFiles {
    # The files of one trace's near-glitch export: trace.etl -> dumper.txt and
    # near.txt; any other <base>.etl (a soak cut) -> <base>.dumper.txt and
    # <base>.near.txt (Invoke-IemDpcIsr's naming).
    param([Parameter(Mandatory)][string]$Dir, [string]$Name = 'trace.etl')
    if ($Name -eq 'trace.etl') { $d = 'dumper.txt'; $n = 'near.txt' }
    else { $b = [IO.Path]::GetFileNameWithoutExtension($Name); $d = "$b.dumper.txt"; $n = "$b.near.txt" }
    [pscustomobject]@{ input = (Join-Path $Dir $Name); dump = (Join-Path $Dir $d); near = (Join-Path $Dir $n) }
}

function Select-IemNearGlitch {
    # The dumper's header plus the DPC/ISR/context-switch rows and the glitch
    # markers, streamed from Dump into Near; returns the number of lines kept.
    param([Parameter(Mandatory)][string]$Dump, [Parameter(Mandatory)][string]$Near)
    $r = New-Object IO.StreamReader($Dump); $w = New-Object IO.StreamWriter($Near, $false, (New-Object Text.UTF8Encoding $false))
    $n = 0; $header = $false
    try {
        while ($null -ne ($line = $r.ReadLine())) {
            $t = $line.Trim()
            if ($t -eq 'BeginHeader') { $header = $true }
            $first = ($t -split ',', 2)[0].Trim()
            if ($header -or $script:NearEvents -contains $first -or $line.Contains('iemmixer-glitch') -or $line.Contains($script:MarkerProvider)) { $w.WriteLine($line); $n++ }
            if ($t -eq 'EndHeader') { $header = $false }
        }
    } finally { $r.Close(); $w.Close() }
    return $n
}

function Export-IemNearGlitch {
    # One trace file's near-glitch rows (-Name, default trace.etl; a soak cut
    # cut-<n>.etl works the same), through its own dumper temp file, which is
    # deleted afterwards.
    param([Parameter(Mandatory)][string]$Xperf, [Parameter(Mandatory)][string]$Dir, [string]$Name = 'trace.etl')
    $f = Get-IemNearGlitchFiles -Dir $Dir -Name $Name
    [void](Invoke-IemXperf -Xperf $Xperf -Arguments @('-i', $f.input, '-o', $f.dump, '-a', 'dumper'))
    $n = Select-IemNearGlitch -Dump $f.dump -Near $f.near
    Remove-Item -LiteralPath $f.dump
    [pscustomobject]@{ lines = $n; path = $f.near }
}

function Get-IemCpuSample {
    # Raw cumulative per-CPU counters (rates are computed on the dev box).
    $raw = @(Get-CimInstance -ClassName Win32_PerfRawData_PerfOS_Processor | Where-Object { $_.Name -match '^\d+$' })
    $fmt = @(Get-CimInstance -ClassName Win32_PerfFormattedData_Counters_ProcessorInformation -ErrorAction SilentlyContinue | Where-Object { $_.Name -notmatch '_Total' })
    [pscustomobject]@{
        cpus = @($raw | ForEach-Object { [pscustomobject]@{
            lp = [int]$_.Name; t100ns = [uint64]$_.Timestamp_Sys100NS; interrupts = [uint64]$_.InterruptsPersec; dpcs = [uint64]$_.DPCsQueuedPersec
            dpc_time = [uint64]$_.PercentDPCTime; int_time = [uint64]$_.PercentInterruptTime; idle_time = [uint64]$_.PercentIdleTime
            c1_time = [uint64]$_.PercentC1Time; c2_time = [uint64]$_.PercentC2Time; c3_time = [uint64]$_.PercentC3Time } })
        freq = @($fmt | ForEach-Object { [pscustomobject]@{ name = $_.Name; mhz = [uint32]$_.ProcessorFrequency; perf_pct = [uint32]$_.PercentProcessorPerformance } })
    }
}

function Get-IemSystemEvents {
    # Warnings and errors of the System log since -Since, by provider and id
    # (no message text: it can name hosts). A failing query throws (A10);
    # -LogName exists for the self-test.
    param([Parameter(Mandatory)][string]$Since, [string]$LogName = 'System')
    $start = [datetime]::Parse($Since, [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::RoundtripKind).ToLocalTime()
    $ev = @(Get-IemWinEvent -Filter @{ LogName = $LogName; Level = 1, 2, 3; StartTime = $start })
    return ,@($ev | Group-Object -Property ProviderName, Id | ForEach-Object { [pscustomobject]@{ provider = $_.Group[0].ProviderName; id = $_.Group[0].Id; count = $_.Count } })
}

function Assert-IemXperf {
    # The xperf at Xperf counts only with a valid Microsoft Authenticode signature
    # and a numeric file version of at least MinVersion (A12); returns that version.
    param([Parameter(Mandatory)][string]$Xperf, [Parameter(Mandatory)][version]$MinVersion)
    try { $sig = Get-AuthenticodeSignature -LiteralPath $Xperf } catch { throw "xperf at ${Xperf}: signature unreadable ($_)" }
    if ("$($sig.Status)" -ne 'Valid') { throw "xperf at ${Xperf}: signature $($sig.Status), not a valid Microsoft signature" }
    if ("$($sig.SignerCertificate.Subject)" -notlike '*O=Microsoft Corporation*') { throw "xperf at ${Xperf}: signed by $($sig.SignerCertificate.Subject), not Microsoft" }
    $vi = (Get-Item -LiteralPath $Xperf).VersionInfo
    $v = New-Object -TypeName version -ArgumentList $vi.FileMajorPart, $vi.FileMinorPart, $vi.FileBuildPart, $vi.FilePrivatePart
    if ($v -lt $MinVersion) { throw "xperf at ${Xperf}: version $v, older than $MinVersion" }
    return "$v"
}

function Install-IemWpt {
    # The ADK bootstrapper (Microsoft-signed) installs only the Windows
    # Performance Toolkit; no reboot, no service, no driver. The xperf it finds
    # or installs must be Microsoft-signed and WPT 10 or newer (the toolkit the
    # report parsers read; -MinVersion raises the floor).
    param([Parameter(Mandatory)][string]$Setup, [Parameter(Mandatory)][string]$Xperf, [version]$MinVersion = $script:XperfMinVersion)
    if (Test-Path -LiteralPath $Xperf) { return [pscustomobject]@{ installed = 'already'; version = (Assert-IemXperf -Xperf $Xperf -MinVersion $MinVersion) } }
    $sig = Get-AuthenticodeSignature -LiteralPath $Setup
    if ($sig.Status -ne 'Valid' -or "$($sig.SignerCertificate.Subject)" -notlike '*O=Microsoft Corporation*') {
        throw "adksetup signature: $($sig.Status) $($sig.SignerCertificate.Subject)"
    }
    $p = Start-Process -FilePath $Setup -ArgumentList '/quiet', '/norestart', '/ceip', 'off', '/features', 'OptionId.WindowsPerformanceToolkit' -PassThru -Wait
    if ($p.ExitCode -ne 0) { throw "adksetup exit $($p.ExitCode)" }
    if (-not (Test-Path -LiteralPath $Xperf)) { throw "xperf not found at $Xperf after the install" }
    [pscustomobject]@{ installed = 'now'; version = (Assert-IemXperf -Xperf $Xperf -MinVersion $MinVersion) }
}

# The REAPER-mode fingerprint (design note 5.1) and inventory M0 (4.2): read
# only, so they live here with the other readers; IemTuning.psm1 (which the
# guard imports alone, elevated) keeps the writers.

function Get-IemTextHash {
    param([AllowEmptyString()][string]$Text)
    $sha = [Security.Cryptography.SHA256]::Create()
    return (($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($Text)) | ForEach-Object { $_.ToString('x2') }) -join '')
}

function Get-IemFileDigest {
    # The file's SHA-256, or of its lines matching any key when keys are given.
    param([Parameter(Mandatory)][string]$Path, [string[]]$Keys = @())
    if (-not (Test-Path -LiteralPath $Path)) { return 'absent' }
    $lines = @(Get-Content -LiteralPath $Path)
    if (@($Keys).Count -gt 0) { $lines = @($lines | Where-Object { $l = $_; @($Keys | Where-Object { $l -match $_ }).Count -gt 0 }) }
    return Get-IemTextHash -Text ($lines -join "`n")
}

function Get-IemRegText {
    param([Parameter(Mandatory)]$Profile, [Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$Name)
    Get-IemValue -Item (New-IemItem -Key 'r' -Kind 'reg' -Arguments @{ path = (Get-IemRegPath $Profile $Path); name = $Name; type = 'String' } -Desired $null)
}

function Get-IemReaperFingerprint {
    # Everything REAPER mode depends on (design note 5.1), read only.
    param([Parameter(Mandatory)][string]$ProfilePath)
    $profile = Read-IemProfile -Path $ProfilePath
    $f = [ordered]@{}
    $f['plan.active'] = [IemPower]::Active()
    $f['plan.reaper.settings'] = Get-IemTextHash -Text ((@(& powercfg.exe /qh $profile.plan.source)) -join "`n")
    $gov = Get-Service -Name $profile.governor -ErrorAction SilentlyContinue
    $f['governor.state'] = $(if ($gov) { "$($gov.Status)" } else { 'absent' })
    $f['governor.start'] = Get-IemValue -Item (New-IemItem -Key 'g' -Kind 'svc-start' -Arguments @{ name = $profile.governor } -Desired $null)
    $n = 0
    foreach ($file in @($profile.fingerprint.files)) { $n++; $f["file.$n"] = Get-IemFileDigest -Path $file -Keys @($profile.fingerprint.keys) }
    $r = @(Get-Process -Name reaper -ErrorAction SilentlyContinue)
    if ($r.Count -eq 1) {
        $f['reaper.priority'] = "$($r[0].PriorityClass)"
        $f['reaper.affinity'] = "$([long]$r[0].ProcessorAffinity)"
        $f['reaper.cpusets'] = ((@([IemCpuSets]::Get($r[0].Id)) | Sort-Object) -join ',')
    } else { $f['reaper.priority'] = "instances=$($r.Count)" }
    $mm = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile'
    foreach ($v in 'SystemResponsiveness', 'NetworkThrottlingIndex') { $f["mmcss.$v"] = Get-IemRegText $profile $mm $v }
    foreach ($v in 'Affinity', 'Background Only', 'Clock Rate', 'GPU Priority', 'Priority', 'Scheduling Category', 'SFIO Priority') {
        $f["mmcss.proaudio.$v"] = Get-IemRegText $profile "$mm\Tasks\Pro Audio" $v
    }
    $f['kernel.ReservedCpuSets'] = Get-IemRegText $profile 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\kernel' 'ReservedCpuSets'
    $f['bcd'] = Get-IemTextHash -Text ((@(& bcdedit.exe /enum '{current}')) -join "`n")
    $dg = Get-CimInstance -Namespace root\Microsoft\Windows\DeviceGuard -ClassName Win32_DeviceGuard -ErrorAction SilentlyContinue
    $f['deviceguard.running'] = $(if ($dg) { (@($dg.SecurityServicesRunning) -join ',') } else { 'unavailable' })
    $f['tuning.entered'] = "$((Read-IemJournal -Path $profile.journal -ModeOnly).entered)"
    return [pscustomobject]$f
}

function Compare-IemFingerprint {
    param([Parameter(Mandatory)]$Baseline, [Parameter(Mandatory)]$Current)
    $names = @(@($Baseline.PSObject.Properties.Name) + @($Current.PSObject.Properties.Name) | Sort-Object -Unique)
    $diff = @()
    foreach ($n in $names) {
        $a = $Baseline.PSObject.Properties[$n]; $b = $Current.PSObject.Properties[$n]
        $va = $(if ($a) { [string]$a.Value } else { '<absent>' }); $vb = $(if ($b) { [string]$b.Value } else { '<absent>' })
        if ($va -ne $vb) { $diff += [pscustomobject]@{ key = $n; baseline = $va; current = $vb } }
    }
    return ,$diff
}

function Get-IemWinEvent {
    # Get-WinEvent where "no events found" is an empty result and every other
    # error (a missing log, access denied, a bad query) throws: a failing query
    # never reads as "no events" (A10).
    param([Parameter(Mandatory)][hashtable]$Filter)
    try { $ev = @(Get-WinEvent -FilterHashtable $Filter -ErrorAction Stop) }
    catch {
        if ("$($_.FullyQualifiedErrorId)" -like 'NoMatchingEventsFound*') { return }
        throw
    }
    return $ev
}

function Get-IemDeviceInventory {
    # PCI devices: driver, MSI and affinity registry values, allocated IRQs
    # (a negative IRQ number is an MSI).
    $irq = try { Get-IemAllocatedIrqs } catch { @{} }
    foreach ($d in @(Get-PnpDevice -PresentOnly | Where-Object { $_.InstanceId -like 'PCI\*' })) {
        $enum = "HKLM:\SYSTEM\CurrentControlSet\Enum\$($d.InstanceId)\Device Parameters\Interrupt Management"
        $read = { param($k, $n) if (Test-Path -LiteralPath $k) { (Get-Item -LiteralPath $k).GetValue($n, $null) } }
        $ver = (Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_DriverVersion' -ErrorAction SilentlyContinue).Data
        $date = (Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_DriverDate' -ErrorAction SilentlyContinue).Data
        [ordered]@{
            instance = $d.InstanceId; name = $d.FriendlyName; class = $d.Class; status = "$($d.Status)"; driver = $ver; driver_date = "$date"
            msi = & $read "$enum\MessageSignaledInterruptProperties" 'MSISupported'
            msi_limit = & $read "$enum\MessageSignaledInterruptProperties" 'MessageNumberLimit'
            policy = & $read "$enum\Affinity Policy" 'DevicePolicy'
            mask = & $read "$enum\Affinity Policy" 'AssignmentSetOverride'
            irqs = @($irq[$d.InstanceId] | Where-Object { $null -ne $_ } | ForEach-Object { [string]$_ })
        }
    }
}

function Get-IemInventory {
    # Inventory M0 (design note 4.2), read only. Never reads process command
    # lines, service image paths or task actions: they can carry tokens.
    param([Parameter(Mandatory)][string]$ProfilePath)
    if (-not ('IemTimer' -as [type])) { Add-Type -TypeDefinition $script:TimerSource }
    $profile = Read-IemProfile -Path $ProfilePath
    $os = Get-CimInstance -ClassName Win32_OperatingSystem
    $bios = Get-CimInstance -ClassName Win32_BIOS
    $map = [IemCpuSets]::Map()
    $tpm = try { Get-Tpm | Select-Object TpmPresent, TpmReady, ManufacturerIdTxt, ManufacturerVersion } catch { "$_" }
    $dg = Get-CimInstance -Namespace root\Microsoft\Windows\DeviceGuard -ClassName Win32_DeviceGuard -ErrorAction SilentlyContinue
    $defender = try { $p = Get-MpPreference; [ordered]@{ exclusion_paths = @($p.ExclusionPath); exclusion_processes = @($p.ExclusionProcess)
                                                         scan_day = $p.ScanScheduleDay; realtime_off = $p.DisableRealtimeMonitoring } } catch { "$_" }
    $since = (Get-Date).AddDays(-365)
    $installed = try { ,@(Get-IemWinEvent -Filter @{ LogName = 'System'; Id = 7045; StartTime = $since } | ForEach-Object {
        [ordered]@{ at = $_.TimeCreated.ToUniversalTime().ToString('o'); service = "$($_.Properties[0].Value)" } }) } catch { "error: $_" }
    $cpusets = [ordered]@{}   # ConvertTo-Json needs string keys
    foreach ($k in ($map.Keys | Sort-Object)) { $cpusets["$k"] = $map[$k] }
    [ordered]@{
        at = (Get-Date).ToUniversalTime().ToString('o')
        os = [ordered]@{ caption = $os.Caption; version = $os.Version; build = $os.BuildNumber; boot = $os.LastBootUpTime.ToUniversalTime().ToString('o') }
        bios = [ordered]@{ vendor = $bios.Manufacturer; version = $bios.SMBIOSBIOSVersion; date = "$($bios.ReleaseDate)" }
        cpu = @(Get-CimInstance -ClassName Win32_Processor | ForEach-Object { [ordered]@{ name = $_.Name; cores = $_.NumberOfCores; logical = $_.NumberOfLogicalProcessors } })
        cpusets = $cpusets
        tpm = $tpm
        deviceguard = $(if ($dg) { [ordered]@{ vbs = $dg.VirtualizationBasedSecurityStatus; running = @($dg.SecurityServicesRunning) } } else { 'unavailable' })
        bcd = @(& bcdedit.exe /enum '{current}')
        timer_100ns = [IemTimer]::Query()
        power = [ordered]@{ active = [IemPower]::Active(); list = @(& powercfg.exe /list); active_settings = @(& powercfg.exe /qh) }
        devices = @(Get-IemDeviceInventory)
        nics = @(Get-NetAdapter | ForEach-Object {
            [ordered]@{ name = $_.Name; description = $_.InterfaceDescription; status = "$($_.Status)"; speed = "$($_.LinkSpeed)"; driver = $_.DriverVersion
                        advanced = @(Get-NetAdapterAdvancedProperty -Name $_.Name -ErrorAction SilentlyContinue | ForEach-Object { [ordered]@{ keyword = $_.RegistryKeyword; value = "$($_.RegistryValue)"; display = $_.DisplayName } })
                        rss = (Get-NetAdapterRss -Name $_.Name -ErrorAction SilentlyContinue | Select-Object Enabled, BaseProcessorNumber, MaxProcessorNumber, MaxProcessors, NumberOfReceiveQueues)
                        pm = (Get-NetAdapterPowerManagement -Name $_.Name -ErrorAction SilentlyContinue | Select-Object AllowComputerToTurnOffDevice) } })
        services = @(Get-CimInstance -ClassName Win32_Service | ForEach-Object { [ordered]@{ name = $_.Name; start = $_.StartMode; state = $_.State } })
        tasks = @(Get-ScheduledTask | Where-Object { "$($_.State)" -ne 'Disabled' } | ForEach-Object {
            $i = $_ | Get-ScheduledTaskInfo -ErrorAction SilentlyContinue
            [ordered]@{ path = $_.TaskPath; name = $_.TaskName; state = "$($_.State)"; last = $(if ($i) { "$($i.LastRunTime)" } else { '' }) } })
        defender = $defender
        processes = @(Get-Process | ForEach-Object {
            $pc = try { "$($_.PriorityClass)" } catch { 'denied' }
            $af = try { "$([long]$_.ProcessorAffinity)" } catch { 'denied' }
            [ordered]@{ name = $_.ProcessName; id = $_.Id; session = $_.SessionId; priority = $pc; affinity = $af } })
        governor_lines = @(foreach ($file in @($profile.fingerprint.files)) { if (Test-Path -LiteralPath $file) {
            @(Get-Content -LiteralPath $file | Where-Object { $_ -match 'IdleSaver|ProBalance|Gaming|Performance|PowerPlan|Priorit|Affinit|CpuSet|SmartTrim|Exclu' }) } })
        mmcss = @(Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile' |
                  Select-Object SystemResponsiveness, NetworkThrottlingIndex)
        history = [ordered]@{
            hotfixes = @(Get-HotFix | ForEach-Object { [ordered]@{ id = $_.HotFixID; installed = "$($_.InstalledOn)" } })
            drivers = @(Get-CimInstance -ClassName Win32_PnPSignedDriver | Where-Object { $_.DriverDate } | ForEach-Object { [ordered]@{ device = $_.DeviceName; version = $_.DriverVersion; date = "$($_.DriverDate)" } })
            services_installed = $installed
        }
    }
}

Export-ModuleMember -Function *-Iem*
