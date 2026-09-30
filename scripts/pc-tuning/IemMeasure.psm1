#Requires -Version 5.1
# S1c measurement on the PC (docs/superpowers/specs/2026-09-27-s1c-windows-tuning-design.md §4.1):
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
    $out = & $Xperf @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) { throw "xperf $($Arguments -join ' ') (exit $LASTEXITCODE): $($out -join ' ')" }
    return ,@($out | ForEach-Object { "$_" })
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

function Export-IemNearGlitch {
    # The dumper's header plus the DPC/ISR/context-switch rows and the glitch
    # markers, streamed into near.txt; the full dump is deleted.
    param([Parameter(Mandatory)][string]$Xperf, [Parameter(Mandatory)][string]$Dir)
    $dump = Join-Path $Dir 'dumper.txt'; $near = Join-Path $Dir 'near.txt'
    [void](Invoke-IemXperf -Xperf $Xperf -Arguments @('-i', (Join-Path $Dir 'trace.etl'), '-o', $dump, '-a', 'dumper'))
    $r = New-Object IO.StreamReader($dump); $w = New-Object IO.StreamWriter($near, $false, (New-Object Text.UTF8Encoding $false))
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
    Remove-Item -LiteralPath $dump
    [pscustomobject]@{ lines = $n; path = $near }
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
    # One sentinel sample (design note §4.1 item 5): CPU counters, the active
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
    # (no message text: it can name hosts).
    param([Parameter(Mandatory)][string]$Since, [string]$LogName = 'System')
    $start = [datetime]::Parse($Since, [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::RoundtripKind).ToLocalTime()
    $ev = @(Get-WinEvent -FilterHashtable @{ LogName = $LogName; Level = 1, 2, 3; StartTime = $start } -ErrorAction SilentlyContinue)
    return ,@($ev | Group-Object -Property ProviderName, Id | ForEach-Object { [pscustomobject]@{ provider = $_.Group[0].ProviderName; id = $_.Group[0].Id; count = $_.Count } })
}

function Install-IemWpt {
    # The ADK bootstrapper (Microsoft-signed) installs only the Windows
    # Performance Toolkit; no reboot, no service, no driver.
    param([Parameter(Mandatory)][string]$Setup, [Parameter(Mandatory)][string]$Xperf)
    if (Test-Path -LiteralPath $Xperf) { return [pscustomobject]@{ installed = 'already'; version = (Get-Item -LiteralPath $Xperf).VersionInfo.FileVersion } }
    $sig = Get-AuthenticodeSignature -LiteralPath $Setup
    if ($sig.Status -ne 'Valid' -or "$($sig.SignerCertificate.Subject)" -notlike '*O=Microsoft Corporation*') {
        throw "adksetup signature: $($sig.Status) $($sig.SignerCertificate.Subject)"
    }
    $p = Start-Process -FilePath $Setup -ArgumentList '/quiet', '/norestart', '/ceip', 'off', '/features', 'OptionId.WindowsPerformanceToolkit' -PassThru -Wait
    if ($p.ExitCode -ne 0) { throw "adksetup exit $($p.ExitCode)" }
    if (-not (Test-Path -LiteralPath $Xperf)) { throw "xperf not found at $Xperf after the install" }
    [pscustomobject]@{ installed = 'now'; version = (Get-Item -LiteralPath $Xperf).VersionInfo.FileVersion }
}

Export-ModuleMember -Function *-Iem*
