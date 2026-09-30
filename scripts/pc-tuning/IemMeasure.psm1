#Requires -Version 5.1
# S1c measurement on the PC (docs/superpowers/specs/2026-09-27-s1c-windows-tuning-design.md section 4.1):
# the xperf kernel trace with the spike's glitch markers, the dpcisr and
# dumper reports, per-CPU counter samples over WMI (language-neutral), the
# System log, the WPT install, and the REAPER-mode fingerprint and inventory.
# Changes no Windows setting.
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
    $f['tuning.entered'] = "$((Read-IemJournal -Path $profile.journal).entered)"
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
