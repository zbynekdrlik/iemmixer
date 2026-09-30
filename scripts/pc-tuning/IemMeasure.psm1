#Requires -Version 5.1
# S1c measurement on the PC (docs/superpowers/specs/2026-09-27-s1c-windows-tuning-design.md section 4.1):
# the xperf kernel trace with the spike's glitch markers, the dpcisr and
# dumper reports, per-CPU counter samples over WMI (language-neutral), the
# System log and the WPT install. Changes no Windows setting.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'IemTuning.psm1') -Force -Global
# crates/iem-audio-io/src/os.rs MARKER_PROVIDER.
$script:MarkerProvider = '3b6c1e0a-5d2f-4c8e-9a71-0e4f2d9b8c11'
$script:MarkerSession = 'IemMarkers'
$script:KernelSession = 'NT Kernel Logger'
$script:NearEvents = @('DPC', 'TimedDPC', 'ThreadedDPC', 'Interrupt', 'CSwitch', 'ReadyThread')
# xperf: WPT 10 or newer (the toolkit the dpcisr/dumper parsers read), Microsoft-signed.
$script:XperfMinVersion = [version]'10.0'
$script:XperfChecked = @{}   # paths verified in this process (m4)

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

function Invoke-IemXperf {
    param([Parameter(Mandatory)][string]$Xperf, [Parameter(Mandatory)][string[]]$Arguments)
    if (-not (Test-Path -LiteralPath $Xperf)) { throw "xperf not found at $Xperf (run wpt-install)" }
    # It runs elevated: only a Microsoft-signed, new-enough binary, checked once per process (m4).
    if (-not $script:XperfChecked.ContainsKey($Xperf)) {
        [void](Assert-IemXperf -Xperf $Xperf -MinVersion $script:XperfMinVersion)
        $script:XperfChecked[$Xperf] = $true
    }
    # stderr is output; the exit code alone decides (A11).
    $r = Invoke-IemNative -FilePath $Xperf -Arguments $Arguments
    if ($r.code -ne 0) { throw "xperf $($Arguments -join ' ') (exit $($r.code)): $($r.out -join ' ')" }
    return ,@($r.out)
}

function ConvertFrom-IemLoggers {
    # Session names from `xperf -Loggers` ("Logger Name : <name>" lines).
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Text)
    return ,@($Text -split "`n" | ForEach-Object { if ($_ -match '^\s*Logger Name\s*:\s*(.+?)\s*$') { $Matches[1] } })
}

function Start-IemTrace {
    param([Parameter(Mandatory)][string]$Xperf, [Parameter(Mandatory)][string]$Dir, [switch]$CSwitch, [int]$CircularMB = 0)
    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    [void](Invoke-IemXperf -Xperf $Xperf -Arguments (New-IemTraceArguments -Dir $Dir -CSwitch:$CSwitch -CircularMB $CircularMB))
    [pscustomobject]@{ dir = $Dir; started = Get-IemNow }
}

function Stop-IemTrace {
    # Stops whichever of the two sessions runs (none is fine: pre-emption may
    # come twice). -Merge merges both into -Name; without it the raw files stay.
    param([Parameter(Mandatory)][string]$Xperf, [Parameter(Mandatory)][string]$Dir, [switch]$Merge, [string]$Name = 'trace.etl')
    $running = ConvertFrom-IemLoggers -Text ((Invoke-IemXperf -Xperf $Xperf -Arguments @('-Loggers')) -join "`n")
    $a = @()
    if ($running -contains $script:KernelSession) { $a += '-stop' }
    if ($running -contains $script:MarkerSession) { $a += @('-stop', $script:MarkerSession) }
    if ($a.Count -eq 0) { return [pscustomobject]@{ stopped = @() } }
    if ($Merge) { $a += @('-d', (Join-Path $Dir $Name)) }
    [void](Invoke-IemXperf -Xperf $Xperf -Arguments $a)
    [pscustomobject]@{ stopped = @($running | Where-Object { @($script:KernelSession, $script:MarkerSession) -contains $_ }) }
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

function Get-IemPollSample {
    # One sentinel sample (design note 4.1 item 5): CPU counters, the active
    # plan, the governor's state and, while a spike runs, the priority of its
    # callback thread (read from outside; the driver's thread is never touched).
    param([Parameter(Mandatory)][string]$ProfilePath, [int]$SpikePid = 0, [int]$ThreadId = 0)
    $profile = Read-IemProfile -Path $ProfilePath
    $gov = Get-Service -Name $profile.governor -ErrorAction SilentlyContinue
    $thread = $null
    if ($SpikePid -gt 0 -and $ThreadId -gt 0) {
        $p = Get-Process -Id $SpikePid -ErrorAction SilentlyContinue
        if ($p) { $t = @($p.Threads | Where-Object { $_.Id -eq $ThreadId }); if ($t.Count -eq 1) { $thread = [pscustomobject]@{ base = $t[0].BasePriority; current = $t[0].CurrentPriority } } }
    }
    [pscustomobject]@{ at = Get-IemNow; cpu = Get-IemCpuSample; plan = [IemPower]::Active(); governor = $(if ($gov) { "$($gov.Status)" } else { 'absent' }); thread = $thread }
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
    $sig = Get-AuthenticodeSignature -LiteralPath $Xperf
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

Export-ModuleMember -Function *-Iem*
