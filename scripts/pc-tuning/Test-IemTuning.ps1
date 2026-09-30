#Requires -Version 5.1
# Self-test of the S1c tuning modules on Windows PowerShell 5.1 (CI job asio-spike,
# an ephemeral administrator runner): real backends: registry values under an
# HKCU test root, two services (Spooler: no start triggers, for the disable-and-stop
# case; W32Time as the governor stand-in), a scheduled task, a duplicated power plan,
# Defender exclusions and the CPU Sets of child processes.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
foreach ($f in (Get-ChildItem -LiteralPath $here -File | Where-Object { @('.ps1', '.psm1') -contains $_.Extension })) {
    $tokens = $null; $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($f.FullName, [ref]$tokens, [ref]$errors)
    if ($errors.Count -gt 0) { throw "parse errors in $($f.Name): $($errors[0].Message)" }
}
Import-Module (Join-Path $here 'IemMeasure.psm1') -Force
function Assert($cond, $what) { if (-not $cond) { throw "FAILED: $what" }; Write-Host "ok  $what" }
# A refusal must throw THIS error: any exception (a typo'd parameter, a missing
# command) would otherwise pass a negative test.
function ThrowsLike([scriptblock]$b, [string]$like, $what) {
    $m = $null
    try { & $b } catch { $m = "$_" }
    if ($null -eq $m) { throw "FAILED: $what (nothing was thrown)" }
    if ($m -notlike $like) { throw "FAILED: $what (threw '$m', expected '$like')" }
    Write-Host "ok  $what"
}
# The module's private readers of external state (a power-scheme name, the
# interrupts Windows granted): the self-test replaces them inside the module;
# no parameter lets a caller bypass them.
function Get-TuningSeam([string]$Name) { & (Get-Module IemTuning) { param($n) Get-Variable -Scope Script -Name $n -ValueOnly -ErrorAction SilentlyContinue } $Name }
function Set-TuningSeam([string]$Name, [scriptblock]$Value) { & (Get-Module IemTuning) { param($n, $v) Set-Variable -Scope Script -Name $n -Value $v } $Name $Value }
# Callers wrap this in @(...) so .Count and a ForEach pipe are array-safe under
# StrictMode on PS 5.1 (a bare (Rows ...) would be $null for 0 matches; a ,@()
# return would make @(Rows ...) iterate once over an empty array; S1c CI).
function Rows($rows, $action) { @($rows | Where-Object { $_.action -eq $action }) }
# Read-IemJournal is exported (every *-Iem* function is); the test reads the flag the module wrote.
function Read-IemJournalState($profilePath) { $p = Read-IemProfile -Path $profilePath; (Read-IemJournal -Path $p.journal).entered }
# The profile version the journal says a tier's last complete apply had ($null when it keeps none).
function Read-JournalVersion($profilePath, [int]$tier) {
    $p = Read-IemProfile -Path $profilePath
    $a = (Read-IemJournal -Path $p.journal)['applied']
    if ($null -eq $a) { return $null }
    return $a["tier$tier"]
}
# Sets the boot identity ({ time, id }) of every global journal entry, as if its
# item had been written in that boot.
function Set-JournalBoot($profilePath, [hashtable]$boot) {
    $jf = (Read-IemProfile -Path $profilePath).journal
    $jo = [IO.File]::ReadAllText($jf) | ConvertFrom-Json
    foreach ($p in @($jo.global.PSObject.Properties)) { $p.Value.boot = $boot }
    [IO.File]::WriteAllText($jf, ($jo | ConvertTo-Json -Depth 8))
}

$id = [guid]::NewGuid().ToString('N')
$root = "HKCU:\Software\iemmixer-tuning-test-$id"
$dir = Join-Path ([IO.Path]::GetTempPath()) "tuning-test-$id"
New-Item -ItemType Directory -Force -Path $dir | Out-Null
foreach ($s in 'Spooler', 'W32Time') {
    $svc = Get-Service -Name $s   # both exist on the runner; a missing one fails the test
    if ($svc.Status -ne 'Running') { Start-Service -InputObject $svc; $svc.WaitForStatus('Running', [TimeSpan]::FromSeconds(60)) }
}
$spoolStart = (Get-Item 'HKLM:\SYSTEM\CurrentControlSet\Services\Spooler').GetValue('Start')
$activeBefore = [IemPower]::Active()
$testPlan = [guid]::NewGuid().ToString()
$foreignPlan = [guid]::NewGuid().ToString()   # an existing plan that is not iemmixer's (M2)
$halfPlan = [guid]::NewGuid().ToString()      # a plan enter cannot name (review 3.3)
$taskPath = '\iemmixer-test\'; $taskName = "t-$id"
Register-ScheduledTask -TaskPath $taskPath -TaskName $taskName -Action (New-ScheduledTaskAction -Execute 'cmd.exe' -Argument '/c exit 0') | Out-Null
$enum = "$root\HKLM\SYSTEM\CurrentControlSet\Enum\PCI\VEN_TEST&DEV_0001\0"
New-Item -Path $enum -Force | Out-Null
# Synthetic PCI ids in the VEN_xxxx&DEV_xxxx form real ones have (hex digits).
$hw = 'PCI\VEN_FFFE&DEV_0001'
New-ItemProperty -LiteralPath $enum -Name 'HardwareID' -PropertyType MultiString -Value @("$hw&SUBSYS_00000001", $hw) | Out-Null
$nic = "$root\HKLM\NIC"
New-Item -Path $nic -Force | Out-Null
# The NIC driver key names the hardware id its driver matched (A8).
New-ItemProperty -LiteralPath $nic -Name 'MatchingDeviceId' -PropertyType String -Value 'pci\ven_fffe&dev_0002' | Out-Null
New-ItemProperty -LiteralPath $nic -Name 'PowerSaving' -PropertyType String -Value '1' | Out-Null
# A value whose name ends like the NDIS keyword *EEE: undoing *EEE must leave it (A2).
New-ItemProperty -LiteralPath $nic -Name 'AdvancedEEE' -PropertyType String -Value '1' | Out-Null
# Values of other kinds than the items write: undo restores their own kind and data (A1).
New-ItemProperty -LiteralPath $nic -Name 'IemDword' -PropertyType DWord -Value 1 | Out-Null
New-ItemProperty -LiteralPath $nic -Name 'IemExpand' -PropertyType ExpandString -Value '%SystemRoot%\iem' | Out-Null
$w = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey("Software\iemmixer-tuning-test-$id\HKLM\NIC", $true)
$w.SetValue('IemQword', [long]7, [Microsoft.Win32.RegistryValueKind]::QWord)
$w.SetValue('IemString', 'text', [Microsoft.Win32.RegistryValueKind]::String)
$w.SetValue('IemMulti0', [string[]]@(), [Microsoft.Win32.RegistryValueKind]::MultiString)
$w.SetValue('IemMulti1', [string[]]@('one'), [Microsoft.Win32.RegistryValueKind]::MultiString)
$w.Close()
$mm = "$root\HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile"
New-Item -Path "$mm\Tasks\Pro Audio" -Force | Out-Null
New-ItemProperty -LiteralPath $mm -Name 'SystemResponsiveness' -PropertyType DWord -Value 0 | Out-Null
$ping = "$env:SystemRoot\System32\PING.EXE"
$child = Start-Process -FilePath $ping -ArgumentList '-n', '240', '127.0.0.1' -PassThru -WindowStyle Hidden

function New-TestNic([string]$Hwid, [string]$Adapter = '', [string]$Key = 'HKLM:\NIC') {
    # The test NIC: its driver key (HKLM:\NIC) under the test root, or with -Adapter found by adapter name.
    $n = [ordered]@{ adapter = 'unused'; key = $Key; hwid = $Hwid; properties = [ordered]@{ PowerSaving = '0'; '*EEE' = '0'; IemDword = '0'; IemExpand = 'plain'
                                                                             IemQword = '8'; IemString = 'other'; IemMulti0 = 'x'; IemMulti1 = 'x' }
                     rss = [ordered]@{ base = 4; max = 5 }; pnp_capabilities = 24 }
    if ($Adapter) { $n.adapter = $Adapter; $n.Remove('key') }
    return $n
}

function New-TestProfile([string]$Hwid, [hashtable]$Set = @{}) {
    $p = [ordered]@{
        version = 1; journal = (Join-Path $dir 'journal.json'); registry_root = $root
        # Disjoint roles on the 4-processor runner, as the window requires.
        layout = [ordered]@{ housekeeping = @(0); nic = @(1); card = @(2); audio = @(3) }
        plan = [ordered]@{ guid = $testPlan; source = $activeBefore }
        governor = 'W32Time'; placement = @('PING'); services_disable = @('Spooler'); services_mode = @()
        updates = [ordered]@{ services = @(); tasks = @() }
        maintenance = [ordered]@{ off = $true; tasks = @("$taskPath$taskName", '\iemmixer-test\no-such-task') }
        defender = [ordered]@{ paths = @($dir); processes = @() }
        devices = @([ordered]@{ id = 'card'; role = 'card'; instance = 'PCI\VEN_TEST&DEV_0001\0'; hwid = $Hwid; lps = @(2); enabled = $true })
        nic = (New-TestNic 'PCI\VEN_FFFE&DEV_0002')
        fingerprint = [ordered]@{ files = @(); keys = @() }
    }
    foreach ($k in @($Set.Keys)) { $p[$k] = $Set[$k] }
    $path = Join-Path $dir "profile-$([guid]::NewGuid().ToString('N')).json"
    [IO.File]::WriteAllText($path, ($p | ConvertTo-Json -Depth 6))
    return $path
}
$pp = New-TestProfile $hw
$maint = "$root\HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Schedule\Maintenance"

try {
    # Journal file: a flushed temp file swapped in; a stop in between leaves the
    # journal missing or empty next to a complete .tmp, which the read uses (A14).
    $jp = Join-Path (Join-Path $dir 'journal-file') 'journal.json'
    # The journal is the module's own (a fresh read), so the test does not depend
    # on its fields; 'entered' marks which write a read returns.
    $jj = Read-IemJournal -Path $jp
    Write-IemJournal -Path $jp -Journal $jj
    Assert (-not (Read-IemJournal -Path $jp).entered -and -not (Test-Path -LiteralPath "$jp.tmp")) 'journal-write-leaves-no-temp-file'
    $jj.entered = $true
    Write-IemJournal -Path $jp -Journal $jj
    Assert ((Read-IemJournal -Path $jp).entered) 'journal-write-replaces-the-journal'
    Move-Item -LiteralPath $jp -Destination "$jp.tmp"
    Assert ((Read-IemJournal -Path $jp).entered) 'journal-read-falls-back-to-a-complete-temp-file'
    [IO.File]::WriteAllText($jp, '')
    Assert ((Read-IemJournal -Path $jp).entered) 'journal-read-falls-back-when-the-journal-is-empty'
    Remove-Item -LiteralPath "$jp.tmp"
    ThrowsLike { Read-IemJournal -Path $jp } '*empty or unreadable*' 'journal-read-refuses-an-empty-journal-without-a-temp-file'

    # Journal schema 2 (m1). A schema-1 journal (written before this review) is read
    # with the exact conversions only: an absent registry before-value becomes raw
    # 'absent', the old plan-exists/plan-value mode entries go (the plan is never
    # reverted now), boot strings become identities without a counter, and the one
    # version becomes "no tier applied". Anything else is refused, naming the file.
    $jv = Join-Path (Join-Path $dir 'journal-v1') 'journal.json'
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $jv) | Out-Null
    $b1 = '2026-01-01T00:00:00.0000000Z'
    $e1 = @{ kind = 'reg'; args = @{ path = 'HKLM:\X'; name = 'V'; type = 'DWord' }; before = $null; tier = 3; group = 'nic'; reboot = $true; at = $b1; boot = $b1 }
    $pe = @{ kind = 'plan-exists'; args = @{ guid = $testPlan; source = $activeBefore }; before = $null; tier = 0; group = 'plan'; reboot = $false; at = $b1; boot = $b1 }
    $pv = @{ kind = 'plan-value'; args = @{ guid = $testPlan; sub = 's'; setting = 's' }; before = '50'; tier = 0; group = 'plan'; reboot = $false; at = $b1; boot = $b1 }
    $pa = @{ kind = 'plan-active'; args = @{}; before = $activeBefore; tier = 0; group = 'plan'; reboot = $false; at = $b1; boot = $b1 }
    $v1 = @{ schema = 1; version = 3; entered = $true; global = @{ 'reg:x' = $e1 }; mode = @{ 'plan:exists' = $pe; 'plan:proc-min' = $pv; 'plan:active' = $pa }
             reverted = @{ 'reg:y' = $b1 }; order = @{ global = @('reg:x'); mode = @('plan:exists', 'plan:proc-min', 'plan:active') } }
    [IO.File]::WriteAllText($jv, ($v1 | ConvertTo-Json -Depth 8))
    $m = Read-IemJournal -Path $jv
    $mx = $m.global['reg:x']
    Assert ($m.schema -eq 2 -and $m.applied.tier2 -eq 0 -and $m.applied.tier3 -eq 0 -and $mx.raw.kind -eq 'absent' -and "$($mx.boot.time)" -eq $b1 -and $null -eq $mx.boot.id) 'journal-v1-converts-the-exact-parts'
    Assert (((@($m.order.mode)) -join ',') -eq 'plan:active' -and -not $m.mode.ContainsKey('plan:exists') -and $m.mode.ContainsKey('plan:active') -and "$($m.reverted['reg:y'].time)" -eq $b1) 'journal-v1-drops-the-old-plan-entries'
    $e1.before = '1'
    [IO.File]::WriteAllText($jv, ($v1 | ConvertTo-Json -Depth 8))
    ThrowsLike { Read-IemJournal -Path $jv } "*$jv*schema 1*reg:x*" 'journal-v1-with-a-registry-value-of-unknown-kind-is-refused'
    # ...but the mode exit ("ide event", logon) needs only its own section: a problem
    # in the global section is reported, never blocks it, and stays refused for
    # everything that touches that section (review 3.2).
    $pj = New-TestProfile $hw @{ journal = $jv }
    $xe = $null; $xr = @()
    try { $xr = Exit-IemTuningMode -ProfilePath $pj } catch { $xe = "$_" }
    $xp = @($xr | Where-Object { $_.action -eq 'problem' })
    Assert ($null -eq $xe -and $xp.Count -eq 1 -and "$($xp[0].error)" -like '*reg:x*') "exit-is-not-blocked-by-a-global-journal-problem ($xe)"
    Assert (-not (Read-IemJournal -Path $jv -ModeOnly).entered) 'exit-with-a-global-problem-clears-entered'
    ThrowsLike { Read-IemJournal -Path $jv } "*$jv*schema 1*reg:x*" 'journal-v1-refusal-survives-an-exit'
    $v1.schema = 9
    [IO.File]::WriteAllText($jv, ($v1 | ConvertTo-Json -Depth 8))
    ThrowsLike { Read-IemJournal -Path $jv } "*$jv*schema 9*" 'journal-of-another-schema-is-refused'

    # A boot is Windows' BootId counter plus the boot time (m5): a reboot bumps BootId
    # however quick it is; a clock step moves the time by seconds; a stuck or missing
    # counter falls back to the time tolerance (A13).
    $now = Get-IemBootIdentity
    $t0 = [string]$now.time
    $t30 = [datetime]::Parse($t0, [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::RoundtripKind).ToUniversalTime().AddSeconds(30).ToString('o')
    Assert ($t0 -and ($null -eq $now.id -or "$($now.id)" -match '^\d+$')) 'boot-identity-reads-time-and-counter'
    Assert (-not (Test-IemSameBoot -A @{ time = $t0; id = 7 } -B @{ time = $t0; id = 8 })) 'boot-a-quick-reboot-is-another-boot'
    Assert (Test-IemSameBoot -A @{ time = $t0; id = 7 } -B @{ time = $t30; id = 7 }) 'boot-a-clock-step-is-the-same-boot'
    Assert (Test-IemSameBoot -A @{ time = $t0; id = $null } -B @{ time = $t30; id = 7 }) 'boot-without-a-counter-uses-the-tolerance'
    Assert (-not (Test-IemSameBoot -A @{ time = $t0; id = 7 } -B @{ time = '2000-01-01T00:00:00.0000000Z'; id = 7 })) 'boot-a-stuck-counter-and-a-far-time-is-another-boot'

    # -Only names groups of the tier: a typo is an error, never an empty apply (A9).
    ThrowsLike { Invoke-IemTuningApply -ProfilePath $pp -Tier 2 -Only @('servics') } '*servics*no tier 2 group*' 'apply-refuses-an-unknown-group'
    ThrowsLike { Undo-IemTuning -ProfilePath $pp -Tier 2 -Only @('servics') } '*servics*no tier 2 group*' 'undo-refuses-an-unknown-group'
    # Overlapping layout roles are refused before any write, as the window does (review R5).
    $po = New-TestProfile $hw @{ layout = [ordered]@{ housekeeping = @(0, 2); nic = @(1); card = @(2); audio = @(3) } }
    ThrowsLike { Invoke-IemTuningApply -ProfilePath $po -Tier 2 -Only @('maintenance') } '*roles overlap*' 'apply-refuses-overlapping-layout-roles'
    ThrowsLike { Enter-IemTuningMode -ProfilePath $po -Only @('governor') } '*roles overlap*' 'enter-refuses-overlapping-layout-roles'
    Assert ((Get-Service W32Time).Status -eq 'Running' -and -not (Read-IemJournalState $pp) -and -not (Test-Path -LiteralPath $maint)) 'layout-refusals-write-nothing'
    # The profile version is stamped only after a complete apply without a failure (A9).
    $rm = Invoke-IemTuningApply -ProfilePath $pp -Tier 2 -Only @('maintenance')
    Assert (@(Rows $rm 'failed').Count -eq 0 -and (Read-JournalVersion $pp 2) -eq 0) 'apply-partial-does-not-stamp-the-version'

    # Tier 2: services, a task (plus an absent one), the maintenance switch, a Defender exclusion.
    $r = Invoke-IemTuningApply -ProfilePath $pp -Tier 2
    Assert (@(Rows $r 'failed').Count -eq 0) "tier2-apply-has-no-failure ($(@(Rows $r 'failed') | ForEach-Object { $_.error }))"
    Assert ((Get-Service Spooler).Status -eq 'Stopped' -and (Get-Item 'HKLM:\SYSTEM\CurrentControlSet\Services\Spooler').GetValue('Start') -eq 4) 'tier2-service-disabled-and-stopped'
    Assert ("$((Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName).State)" -eq 'Disabled') 'tier2-task-disabled'
    Assert (@(Rows $r 'absent').Count -eq 1) 'tier2-a-missing-task-is-absent-not-an-error'
    Assert ((Get-Item -LiteralPath $maint).GetValue('MaintenanceDisabled') -eq 1) 'tier2-maintenance-off'
    Assert (@((Get-MpPreference).ExclusionPath) -contains $dir) 'tier2-defender-exclusion'
    Assert ((Read-JournalVersion $pp 2) -eq 1) 'apply-complete-stamps-the-version'
    $again = Invoke-IemTuningApply -ProfilePath $pp -Tier 2
    Assert (@(Rows $again 'written').Count -eq 0 -and @(Rows $again 'failed').Count -eq 0) 'tier2-apply-is-idempotent'
    # An exclusion Defender cannot hold (an empty path) fails its row: version 2 is not stamped.
    $pf = New-TestProfile $hw @{ version = 2; defender = [ordered]@{ paths = @($dir, ''); processes = @() } }
    $rf = Invoke-IemTuningApply -ProfilePath $pf -Tier 2
    Assert (@(Rows $rf 'failed').Count -eq 1 -and (Read-JournalVersion $pp 2) -eq 1) 'apply-with-a-failure-does-not-stamp-the-version'
    $u = Undo-IemTuning -ProfilePath $pp -Tier 2
    Assert (@(Rows $u 'failed').Count -eq 0) 'tier2-undo-has-no-failure'
    Assert ((Get-Service Spooler).Status -eq 'Running' -and (Get-Item 'HKLM:\SYSTEM\CurrentControlSet\Services\Spooler').GetValue('Start') -eq $spoolStart) 'tier2-undo-restores-the-original'
    Assert ("$((Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName).State)" -ne 'Disabled') 'tier2-undo-enables-the-task'
    Assert ($null -eq (Get-Item -LiteralPath $maint).GetValue('MaintenanceDisabled', $null)) 'tier2-undo-deletes-absent-values'
    Assert (-not (@((Get-MpPreference).ExclusionPath) -contains $dir)) 'tier2-undo-removes-the-exclusion'

    # Tier 3: affinity policy under the device's key, NIC values; pending until a reboot.
    # Hardware ids are compared exactly (case-insensitive), never as a pattern: an
    # empty, wildcard, short or other id is refused before any write (M1).
    foreach ($c in @(@('', '*empty*'), @('*', '*wildcard*'), @('PCI\VEN_FFFE&DEV_000?', '*wildcard*'),
                     @('PCI\VEN_FFFE', '*not a PCI VEN_/DEV_*'), @('PCI\VEN_FFFE&DEV_0002', '*hardware id does not match*'))) {
        $bp = New-TestProfile $c[0]
        ThrowsLike { Invoke-IemTuningApply -ProfilePath $bp -Tier 3 -Only @('irq') } $c[1] "tier3-refuses-the-card-hwid '$($c[0])'"
    }
    # A device's processors must exist, and the card's must be the layout's card
    # role, before its affinity is written (review 3.9); "lps": null is no
    # processor, never processor 0 (review R3).
    foreach ($c in @(@(@(2, 62), @(2, 62), '*not present*'), @(@(2), @(2, 62), '*layout.card*'), @(@(), @(2), '*no processors*'),
                     @($null, @(2), '*no processors*'))) {
        $dv = @([ordered]@{ id = 'card'; role = 'card'; instance = 'PCI\VEN_TEST&DEV_0001\0'; hwid = $hw; lps = $c[0]; enabled = $true })
        $bp = New-TestProfile $hw @{ devices = $dv; layout = [ordered]@{ housekeeping = @(0); nic = @(1); card = $c[1]; audio = @(3) } }
        ThrowsLike { Invoke-IemTuningApply -ProfilePath $bp -Tier 3 -Only @('irq') } $c[2] "tier3-refuses-card-processors '$($c[0] -join ',')'"
    }
    # The card is the device with role 'card', whatever its id; no other device may
    # sit on the card's or the audio processors (design note 6.4 R3); one card at
    # most (review R4).
    $ti = 'PCI\VEN_TEST&DEV_0001\0'
    $dsp = [ordered]@{ id = 'dsp'; role = 'card'; instance = $ti; hwid = $hw; lps = @(1); enabled = $true }
    $crd = [ordered]@{ id = 'card'; role = 'card'; instance = $ti; hwid = $hw; lps = @(2); enabled = $true }
    $crd2 = [ordered]@{ id = 'card2'; role = 'card'; instance = $ti; hwid = $hw; lps = @(2); enabled = $true }
    $usb2 = [ordered]@{ id = 'usb'; instance = $ti; hwid = $hw; lps = @(2); enabled = $true }
    $usb3 = [ordered]@{ id = 'usb'; instance = $ti; hwid = $hw; lps = @(3); enabled = $true }
    foreach ($c in @(@(@($dsp), '*layout.card*'), @(@($crd, $usb2), '*card or audio*'), @(@($crd, $usb3), '*card or audio*'), @(@($crd, $crd2), '*one card*'))) {
        $bp = New-TestProfile $hw @{ devices = $c[0] }
        ThrowsLike { Invoke-IemTuningApply -ProfilePath $bp -Tier 3 -Only @('irq') } $c[1] "tier3-refuses-device-roles $($c[1])"
    }
    Assert (-not (Test-Path -LiteralPath "$enum\Device Parameters")) 'tier3-refusal-writes-nothing'
    # The NIC driver key is checked against the profile's hardware id before any write,
    # and found under registry_root also by adapter name (design note 7, A8).
    foreach ($c in @(@('', '*empty*'), @('*', '*wildcard*'), @('PCI\VEN_FFFE', '*hardware id does not match*'),
                     @('PCI\VEN_OTHER', '*hardware id does not match*'))) {
        $bp = New-TestProfile $hw @{ nic = (New-TestNic $c[0]) }
        ThrowsLike { Invoke-IemTuningApply -ProfilePath $bp -Tier 3 -Only @('nic') } $c[1] "tier3-refuses-the-nic-hwid '$($c[0])'"
    }
    $nk = Get-Item -LiteralPath $nic
    Assert ($nk.GetValue('PowerSaving') -eq '1' -and $null -eq $nk.GetValue('*RssBaseProcNumber', $null)) 'tier3-nic-refusal-writes-nothing'
    $an = @(Get-NetAdapter)[0]
    $cls = "$root\HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e972-e325-11ce-bfc1-08002be10318}\0000"
    New-Item -Path $cls -Force | Out-Null
    New-ItemProperty -LiteralPath $cls -Name 'NetCfgInstanceId' -PropertyType String -Value "$($an.InterfaceGuid)" | Out-Null
    New-ItemProperty -LiteralPath $cls -Name 'MatchingDeviceId' -PropertyType String -Value 'pci\ven_fffe&dev_0002' | Out-Null
    $byName = Read-IemProfile -Path (New-TestProfile $hw @{ nic = (New-TestNic 'PCI\VEN_FFFE&DEV_0002' $an.Name) })
    Assert ("$(Get-IemNicKey -Profile $byName)" -like "*iemmixer-tuning-test-$id*") 'tier3-nic-by-adapter-name-stays-under-registry-root'
    # R1 applies only while the card already uses MSI (design note 6.4 R1): otherwise
    # it is skipped with its reason, and nothing is written (A7).
    $rs = Invoke-IemTuningApply -ProfilePath $pp -Tier 3 -Only @('irq')
    $sk = @(Rows $rs 'skipped')
    Assert ($sk.Count -eq 1 -and "$($sk[0].value)" -like '*line-based*' -and @(Rows $rs 'written').Count -eq 0 -and -not (Test-Path -LiteralPath "$enum\Device Parameters")) 'tier3-skips-the-card-without-msi'
    $msiKey = "$enum\Device Parameters\Interrupt Management\MessageSignaledInterruptProperties"
    New-Item -Path $msiKey -Force | Out-Null
    New-ItemProperty -LiteralPath $msiKey -Name 'MSISupported' -PropertyType DWord -Value 1 | Out-Null
    # MSI in use = the flag AND message-signaled interrupts granted now (a negative IRQ
    # number in Win32_PnPAllocatedResource); INTx granted or none granted is skipped (m3).
    # The grants come from the module's private reader, which the self-test replaces;
    # no parameter lets a caller skip the Win32_PnPAllocatedResource read (review 3.5).
    Assert (-not (Get-Command Invoke-IemTuningApply).Parameters.ContainsKey('AllocatedIrqs') -and -not (Get-Command Get-IemGlobalItems).Parameters.ContainsKey('AllocatedIrqs')) 'msi-grant-read-has-no-bypass-parameter'
    $savedIrqs = Get-TuningSeam 'ReadAllocatedIrqs'
    $inst = 'PCI\VEN_TEST&DEV_0001\0'
    foreach ($c in @(@(@{ $inst = @(16) }, '*INTx*'), @(@{ $inst = @(-3, 17) }, '*INTx*'), @(@{}, '*no interrupt*'))) {
        $grant = $c[0]
        Set-TuningSeam 'ReadAllocatedIrqs' ({ $grant }.GetNewClosure())
        $rg = Invoke-IemTuningApply -ProfilePath $pp -Tier 3 -Only @('irq')
        $sk = @(Rows $rg 'skipped')
        Assert ($sk.Count -eq 1 -and "$($sk[0].value)" -like $c[1] -and @(Rows $rg 'written').Count -eq 0 -and -not (Test-Path -LiteralPath "$enum\Device Parameters\Interrupt Management\Affinity Policy")) "tier3-skips-the-card-without-granted-msi $($c[1])"
    }
    $ai = Get-IemAllocatedIrqs
    Assert ($ai -is [hashtable]) 'allocated-irqs-read-from-wmi'
    # The card's mask exists as REG_BINARY with another mask; undo restores exactly it (A1).
    $apKey = "$enum\Device Parameters\Interrupt Management\Affinity Policy"
    New-Item -Path $apKey -Force | Out-Null
    New-ItemProperty -LiteralPath $apKey -Name 'AssignmentSetOverride' -PropertyType Binary -Value ([byte[]](8, 0, 0, 0, 0, 0, 0, 0)) | Out-Null
    $grant = @{ $inst = @(-3, -2) }
    Set-TuningSeam 'ReadAllocatedIrqs' ({ $grant }.GetNewClosure())
    $r3 = Invoke-IemTuningApply -ProfilePath $pp -Tier 3
    Set-TuningSeam 'ReadAllocatedIrqs' $savedIrqs
    Assert (@(Rows $r3 'failed').Count -eq 0) 'tier3-apply-has-no-failure'
    $ap = Get-Item -LiteralPath "$enum\Device Parameters\Interrupt Management\Affinity Policy"
    # The mask is written as REG_BINARY, the KAFFINITY's canonical form: 8 bytes, little
    # endian (M3); processor 2 is the card's role, mask 4.
    $apBytes = (@($ap.GetValue('AssignmentSetOverride') | ForEach-Object { $_.ToString('x2') }) -join '')
    Assert ($ap.GetValue('DevicePolicy') -eq 4 -and "$($ap.GetValueKind('AssignmentSetOverride'))" -eq 'Binary' -and $apBytes -eq '0400000000000000') 'tier3-affinity-policy-and-binary-mask'
    Assert ((Get-Item -LiteralPath $nic).GetValue('PowerSaving') -eq '0' -and (Get-Item -LiteralPath $nic).GetValue('*RssBaseProcNumber') -eq '4') 'tier3-nic-values'
    $st = Get-IemTuningState -ProfilePath $pp
    Assert (@($st.items | Where-Object { $_.tier -eq 3 -and -not $_.pending }).Count -eq 0) 'tier3-items-are-pending-until-a-reboot'
    # A clock step (time sync) moves LastBootUpTime; the boot stays the same (A13).
    $bj = (Read-IemJournal -Path (Read-IemProfile -Path $pp).journal).global['irq:card:policy'].boot
    $bn = Get-IemBootIdentity
    Assert ((Test-IemSameBoot -A $bj -B $bn) -and "$($bj.id)" -eq "$($bn.id)") 'tier3-journals-the-boot-identity'
    $now = Get-IemBootIdentity
    $step = [datetime]::Parse([string]$now.time, [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::RoundtripKind).ToUniversalTime().AddSeconds(30).ToString('o')
    Set-JournalBoot $pp @{ time = $step; id = $now.id }
    $st = Get-IemTuningState -ProfilePath $pp
    Assert (@($st.items | Where-Object { $_.tier -eq 3 -and -not $_.pending }).Count -eq 0) 'tier3-pending-survives-a-clock-step'
    # Pending means written after the current boot: the boot of the latest write counts (A3).
    Set-JournalBoot $pp @{ time = '2000-01-01T00:00:00.0000000Z'; id = $null }
    $st = Get-IemTuningState -ProfilePath $pp
    Assert (@($st.items | Where-Object { $_.tier -eq 3 -and $_.pending }).Count -eq 0) 'tier3-an-earlier-boot-is-not-pending'
    Set-ItemProperty -LiteralPath $nic -Name 'PowerSaving' -Value '1'
    $rw = Invoke-IemTuningApply -ProfilePath $pp -Tier 3 -Only @('nic')
    Assert (@(Rows $rw 'written').Count -eq 1 -and @(Rows $rw 'failed').Count -eq 0) 'tier3-reapply-rewrites-a-changed-value'
    $st = Get-IemTuningState -ProfilePath $pp
    $again3 = @($st.items | Where-Object { $_.pending } | ForEach-Object { $_.key })
    Assert ($again3.Count -eq 1 -and $again3[0] -eq 'nic:PowerSaving') 'tier3-a-rewrite-is-pending-again'
    # One applied version per tier (m2): a complete Tier 2 apply of profile version 2
    # leaves Tier 3, applied at version 1, drifting.
    $pv2 = New-TestProfile $hw @{ version = 2 }
    $r22 = Invoke-IemTuningApply -ProfilePath $pv2 -Tier 2
    $s2 = Get-IemTuningState -ProfilePath $pv2
    Assert (@(Rows $r22 'failed').Count -eq 0 -and $s2.drift -and @($s2.drift_tiers) -contains 3 -and @($s2.drift_tiers) -notcontains 2) 'drift-is-per-tier'
    $u22 = Undo-IemTuning -ProfilePath $pv2 -Tier 2
    Assert (@(Rows $u22 'failed').Count -eq 0 -and (Get-Service Spooler).Status -eq 'Running') 'drift-test-undoes-its-tier2'
    # A journal entry names its target: when the same key now points elsewhere (the
    # NIC's driver key recreated, a card in another slot), the write is refused and
    # the operator told to undo first; no before-value goes unjournaled (review 3.8).
    $nic2 = "$root\HKLM\NIC2"
    New-Item -Path $nic2 -Force | Out-Null
    New-ItemProperty -LiteralPath $nic2 -Name 'MatchingDeviceId' -PropertyType String -Value 'pci\ven_fffe&dev_0002' | Out-Null
    New-ItemProperty -LiteralPath $nic2 -Name 'PowerSaving' -PropertyType String -Value '1' | Out-Null
    $pn2 = New-TestProfile $hw @{ nic = (New-TestNic 'PCI\VEN_FFFE&DEV_0002' '' 'HKLM:\NIC2') }
    $rn2 = Invoke-IemTuningApply -ProfilePath $pn2 -Tier 3 -Only @('nic')
    $fn2 = @(Rows $rn2 'failed')
    Assert (@(Rows $rn2 'written').Count -eq 0 -and $fn2.Count -gt 0 -and "$($fn2[0].error)" -like '*undo*first*' -and (Get-Item -LiteralPath $nic2).GetValue('PowerSaving') -eq '1' -and $null -eq (Get-Item -LiteralPath $nic2).GetValue('*EEE', $null)) 'tier3-refuses-a-journaled-key-whose-target-moved'
    $u3 = Undo-IemTuning -ProfilePath $pp -Tier 3
    Assert (@(Rows $u3 'failed').Count -eq 0) 'tier3-undo-has-no-failure'
    Assert ($null -eq $ap.GetValue('DevicePolicy', $null) -and (Get-Item -LiteralPath $nic).GetValue('PowerSaving') -eq '1') 'tier3-undo-deletes-absent-values'
    $nk = Get-Item -LiteralPath $nic
    Assert ($null -eq $nk.GetValue('*EEE', $null) -and $nk.GetValue('AdvancedEEE') -eq '1') 'tier3-undo-deletes-exactly-the-literal-value'
    $apv = Get-Item -LiteralPath $apKey
    Assert ("$($apv.GetValueKind('AssignmentSetOverride'))" -eq 'Binary' -and (@($apv.GetValue('AssignmentSetOverride') | ForEach-Object { $_.ToString('x2') }) -join '') -eq '0800000000000000') 'tier3-undo-restores-a-binary-value'
    Assert ("$($nk.GetValueKind('IemDword'))" -eq 'DWord' -and $nk.GetValue('IemDword') -eq 1) 'tier3-undo-restores-a-dword-value'
    Assert ("$($nk.GetValueKind('IemExpand'))" -eq 'ExpandString' -and $nk.GetValue('IemExpand', $null, 'DoNotExpandEnvironmentNames') -eq '%SystemRoot%\iem') 'tier3-undo-restores-an-expand-string'
    Assert ("$($nk.GetValueKind('IemQword'))" -eq 'QWord' -and $nk.GetValue('IemQword') -eq 7) 'tier3-undo-restores-a-qword-value'
    Assert ("$($nk.GetValueKind('IemString'))" -eq 'String' -and $nk.GetValue('IemString') -ceq 'text') 'tier3-undo-restores-a-string-value'
    $m0 = @($nk.GetValue('IemMulti0')); $m1 = @($nk.GetValue('IemMulti1'))
    Assert ("$($nk.GetValueKind('IemMulti0'))" -eq 'MultiString' -and $m0.Count -eq 0) 'tier3-undo-restores-an-empty-multi-string'
    Assert ("$($nk.GetValueKind('IemMulti1'))" -eq 'MultiString' -and $m1.Count -eq 1 -and $m1[0] -ceq 'one') 'tier3-undo-restores-a-one-element-multi-string'
    $st = Get-IemTuningState -ProfilePath $pp
    Assert (@($st.items | Where-Object { $_.key -eq 'irq:card:policy' -and $_.revert_pending }).Count -eq 1) 'tier3-undo-is-pending-until-a-reboot'

    # Plan values go only into iemmixer's own plan (M2): the REAPER-mode plan
    # (plan.source), a built-in scheme or another existing plan is refused before
    # any write (no governor pause, no 'entered', no value in any plan).
    $proc = '54533251-82be-4824-96c1-47b60b740d00'; $procMin = '893dee8e-2bef-41e0-89c6-b55d0929964c'
    $builtin = @(@('381b4222-f694-41f0-9685-ff5bb260df2e', '8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c', 'a1841308-3541-4fab-bc81-f71556f20b4a') | Where-Object { $_ -ne $activeBefore })[0]
    [void](& powercfg.exe /duplicatescheme $activeBefore $foreignPlan)
    $srcMin = [IemPower]::Read($activeBefore, $proc, $procMin); $foreignMin = [IemPower]::Read($foreignPlan, $proc, $procMin)
    foreach ($c in @(@($activeBefore, '*REAPER-mode plan*'), @($builtin, '*built-in*'), @($foreignPlan, "*not iemmixer*"))) {
        $bp = New-TestProfile $hw @{ plan = [ordered]@{ guid = $c[0]; source = $activeBefore } }
        ThrowsLike { Enter-IemTuningMode -ProfilePath $bp -Only @('plan', 'governor') -Idle 'disable' } $c[1] "enter-refuses-the-plan $($c[0])"
    }
    Assert ([IemPower]::Active() -eq $activeBefore -and [IemPower]::Read($activeBefore, $proc, $procMin) -eq $srcMin -and [IemPower]::Read($foreignPlan, $proc, $procMin) -eq $foreignMin -and (Get-Service W32Time).Status -eq 'Running' -and -not (Read-IemJournalState $pp)) 'enter-plan-refusals-write-nothing'
    # The second M2 net: a plan value is never written into a plan that is not
    # iemmixer's, even by a direct write (review 3.4).
    $otherMin = $(if ($foreignMin -eq 37) { 38 } else { 37 })
    $pvItem = [pscustomobject]@{ key = 'plan:proc-min'; kind = 'plan-value'; args = @{ guid = $foreignPlan; sub = $proc; setting = $procMin } }
    ThrowsLike { Set-IemValue -Item $pvItem -Value "$otherMin" } "*not iemmixer's own plan*" 'plan-value-refuses-a-foreign-plan'
    Assert ([IemPower]::Read($foreignPlan, $proc, $procMin) -eq $foreignMin) 'plan-value-refusal-writes-nothing'
    # The plan-exists writer checks its GUID itself too (review R7): never plan.source,
    # never a built-in scheme, never a plan that already exists, so neither
    # /duplicatescheme nor the cleanup /delete can touch a plan it did not create.
    $planNames = { @(foreach ($g in $activeBefore, $builtin, $foreignPlan) { "$g=$(Get-IemPlanName -Guid $g)" }) -join '|' }
    $namesBefore = & $planNames
    foreach ($c in @(@($activeBefore, '*REAPER-mode plan*'), @($builtin, '*built-in*'), @($foreignPlan, '*already exists*'))) {
        $peItem = [pscustomobject]@{ key = 'plan:exists'; kind = 'plan-exists'; args = @{ guid = $c[0]; source = $activeBefore } }
        ThrowsLike { Set-IemValue -Item $peItem -Value 'present' } $c[1] "plan-exists-refuses $($c[0])"
    }
    $builtinValue = [IemPower]::Read($builtin, $proc, $procMin)
    ThrowsLike { Set-IemValue -Item ([pscustomobject]@{ key = 'plan:proc-min'; kind = 'plan-value'; args = @{ guid = $builtin; sub = $proc; setting = $procMin } }) -Value '37' } '*built-in*' 'plan-value-refuses-a-built-in-scheme'
    Assert ((& $planNames) -ceq $namesBefore -and [IemPower]::Read($builtin, $proc, $procMin) -eq $builtinValue -and [IemPower]::Active() -eq $activeBefore) 'plan-writer-refusals-change-no-plan'

    # Mode levers: plan (C1 only), governor stand-in, placement.
    $e = Enter-IemTuningMode -ProfilePath $pp -Only @('plan', 'governor', 'placement') -Idle 'c1'
    Assert (@(Rows $e 'failed').Count -eq 0) "enter-has-no-failure ($(@(Rows $e 'failed') | ForEach-Object { $_.key + ': ' + $_.error }))"
    Assert ([IemPower]::Active() -eq $testPlan) 'enter-activates-the-plan'
    Assert (@(& powercfg.exe /list) -match "$testPlan.*\(iemmixer\)") 'enter-names-its-plan-iemmixer'
    Assert ([IemPower]::Read($testPlan, '54533251-82be-4824-96c1-47b60b740d00', '9943e905-9a30-4ec1-9b99-44dd3b76f7a2') -eq 1) 'enter-limits-idle-to-c1'
    Assert ([IemPower]::Read($testPlan, '54533251-82be-4824-96c1-47b60b740d00', '893dee8e-2bef-41e0-89c6-b55d0929964c') -eq 100) 'enter-sets-processor-min-100'
    Assert ((Get-Service W32Time).Status -eq 'Stopped') 'enter-pauses-the-governor'
    # Process Lasso's governor is paused before the iemmixer plan activates, and on exit
    # restarts only after the REAPER-mode plan is active again (A5).
    $ek = @($e | ForEach-Object { $_.key })
    Assert ([array]::IndexOf($ek, 'governor') -ge 0 -and [array]::IndexOf($ek, 'governor') -lt [array]::IndexOf($ek, 'plan:active')) 'enter-pauses-the-governor-before-the-plan'
    $hk = [IemCpuSets]::Map()[0]
    Assert ((@([IemCpuSets]::Get($child.Id)) -join ',') -eq "$hk") 'enter-places-the-process'
    $e2 = Enter-IemTuningMode -ProfilePath $pp -Only @('plan', 'governor', 'placement') -Idle 'c1'
    Assert (@(Rows $e2 'written').Count -eq 0) 'enter-is-idempotent'
    # A new session restores from the journal alone.
    Remove-Module IemMeasure, IemTuning
    Import-Module (Join-Path $here 'IemMeasure.psm1') -Force
    $x = Exit-IemTuningMode -ProfilePath $pp
    Assert ([IemPower]::Active() -eq $activeBefore) 'exit-from-journal-in-a-new-session'
    $xk = @($x | ForEach-Object { $_.key })
    Assert ([array]::IndexOf($xk, 'plan:active') -ge 0 -and [array]::IndexOf($xk, 'plan:active') -lt [array]::IndexOf($xk, 'governor')) 'exit-restores-the-plan-before-the-governor'
    # The plan stays defined but inactive (design note 6.2 L2); the next enter reuses it (A6).
    Assert (@(& powercfg.exe /list) -match $testPlan) 'exit-keeps-the-plan-defined'
    Assert ((Get-Service W32Time).Status -eq 'Running') 'exit-restarts-the-governor'
    Assert ((@([IemCpuSets]::Get($child.Id)) -join ',') -eq '') 'exit-clears-the-placement'
    Assert (-not (Read-IemJournalState $pp)) 'exit-clears-entered'
    Assert ((Exit-IemTuningMode -ProfilePath $pp).Count -eq 0) 'exit-twice-is-harmless'
    $er = Enter-IemTuningMode -ProfilePath $pp -Only @('plan') -Idle 'c1'
    $ew = @(Rows $er 'written' | ForEach-Object { $_.key })
    Assert (@(Rows $er 'failed').Count -eq 0 -and @(Rows $er 'kept' | Where-Object { $_.key -eq 'plan:exists' }).Count -eq 1 -and $ew.Count -eq 1 -and $ew[0] -eq 'plan:active') 'enter-reuses-the-plan'
    # New values written into the active plan take effect only through PowerSetActiveScheme (A4).
    $ea = Enter-IemTuningMode -ProfilePath $pp -Only @('plan') -Idle 'disable'
    $ra = @($ea | Where-Object { $_.key -eq 'plan:active' })
    Assert (@(Rows $ea 'failed').Count -eq 0 -and $ra.Count -eq 1 -and $ra[0].action -eq 'reactivated' -and [IemPower]::Active() -eq $testPlan -and [IemPower]::Read($testPlan, '54533251-82be-4824-96c1-47b60b740d00', '5d76a2ca-e8c0-402f-a133-2158492d58ad') -eq 1) 'enter-reactivates-the-active-plan-after-new-values'
    [void](Exit-IemTuningMode -ProfilePath $pp)
    Assert ([IemPower]::Active() -eq $activeBefore -and (@(& powercfg.exe /list) -match $testPlan)) 'exit-after-a-reuse-keeps-the-plan-inactive'
    # A plan whose name cannot be read is never taken for "no such plan" (review
    # 3.1): an existing plan without a readable name is refused before any write.
    $savedName = Get-TuningSeam 'ReadPlanName'
    Set-TuningSeam 'ReadPlanName' { param($g) $null }
    ThrowsLike { Enter-IemTuningMode -ProfilePath $pp -Only @('plan') -Idle 'c1' } '*name cannot be read*' 'enter-refuses-an-existing-plan-without-a-readable-name'
    Set-TuningSeam 'ReadPlanName' { param($g) throw 'simulated name read failure' }
    ThrowsLike { Get-IemPlanName -Guid $testPlan } '*name cannot be read*' 'plan-name-read-failure-throws-for-an-existing-plan'
    Assert ($null -eq (Get-IemPlanName -Guid ([guid]::NewGuid().ToString()))) 'plan-name-read-failure-of-a-missing-plan-is-null'
    Set-TuningSeam 'ReadPlanName' $savedName
    Assert ($null -eq (Get-IemPlanName -Guid ([guid]::NewGuid().ToString())) -and (Get-IemPlanName -Guid $testPlan) -ceq 'iemmixer') 'plan-name-reads-the-real-name'
    Assert (-not (Read-IemJournalState $pp) -and [IemPower]::Active() -eq $activeBefore) 'plan-name-refusals-write-nothing'
    # The plan enter creates is named at once; when the name does not read back, the
    # plan this call just created is deleted and the row says so (review 3.3). This
    # read-back is also the second M2 safety net (3.4).
    $ph = New-TestProfile $hw @{ plan = [ordered]@{ guid = $halfPlan; source = $activeBefore } }
    Set-TuningSeam 'ReadPlanName' { param($g) if (@(& powercfg.exe /list) -match $g) { 'not-iemmixer' } }
    $eh = Enter-IemTuningMode -ProfilePath $ph -Only @('plan') -Idle 'c1'
    Set-TuningSeam 'ReadPlanName' $savedName
    $hx = @($eh | Where-Object { $_.key -eq 'plan:exists' })
    Assert ($hx.Count -eq 1 -and $hx[0].action -eq 'failed' -and "$($hx[0].error)" -like '*just created was deleted*' -and -not (@(& powercfg.exe /list) -match $halfPlan) -and [IemPower]::Active() -eq $activeBefore) 'enter-deletes-a-plan-it-could-not-name'
    [void](Exit-IemTuningMode -ProfilePath $ph)

    # A placed process that ended is skipped; a reused pid is refused.
    $short = Start-Process -FilePath $ping -ArgumentList '-n', '5', '127.0.0.1' -PassThru -WindowStyle Hidden
    [void](Enter-IemTuningMode -ProfilePath $pp -Only @('placement'))
    $short.WaitForExit(10000) | Out-Null
    $x = Exit-IemTuningMode -ProfilePath $pp
    Assert (@(Rows $x 'gone').Count -ge 1) 'exit-skips-a-process-that-ended'
    ThrowsLike { Set-IemValue -Item ([pscustomobject]@{ key = 'k'; kind = 'cpusets'; args = @{ pid = $child.Id; name = 'PING'; start = 1 } }) -Value '' } '*pid was reused*' 'cpusets-refuse-a-reused-pid'
    # A placement entry belongs to the process that started at its args.start: a later
    # process with the same name and pid is never placed under it, since the restore
    # would find the entry's process 'gone' and leave the later one placed (review R6).
    $pk = "placement:PING:$($child.Id)"
    [void](Enter-IemTuningMode -ProfilePath $pp -Only @('placement'))
    $jf = (Read-IemProfile -Path $pp).journal
    $jj = Read-IemJournal -Path $jf
    $jj.mode[$pk].args.start = [long]$jj.mode[$pk].args.start - 1   # the entry of an earlier process
    Write-IemJournal -Path $jf -Journal $jj
    $childArgs = @{ pid = $child.Id; name = 'PING'; start = $child.StartTime.ToUniversalTime().Ticks }
    Set-IemValue -Item ([pscustomobject]@{ key = $pk; kind = 'cpusets'; args = $childArgs }) -Value ''
    $e6 = Enter-IemTuningMode -ProfilePath $pp -Only @('placement')
    $r6 = @($e6 | Where-Object { $_.key -eq $pk })
    Assert ($r6.Count -eq 1 -and $r6[0].action -eq 'failed' -and "$($r6[0].error)" -like '*earlier process*' -and (@([IemCpuSets]::Get($child.Id)) -join ',') -eq '') 'enter-refuses-a-placement-journaled-for-an-earlier-process'
    $x6 = Exit-IemTuningMode -ProfilePath $pp
    $g6 = @($x6 | Where-Object { $_.key -eq $pk })
    Assert ($g6.Count -eq 1 -and $g6[0].action -eq 'gone' -and (@([IemCpuSets]::Get($child.Id)) -join ',') -eq '' -and -not (Read-IemJournalState $pp)) 'exit-leaves-the-later-process-untouched'

    # Fingerprint: stable, and a change is named.
    $f1 = Get-IemReaperFingerprint -ProfilePath $pp
    Assert ((Compare-IemFingerprint -Baseline $f1 -Current (Get-IemReaperFingerprint -ProfilePath $pp)).Count -eq 0) 'fingerprint-is-stable'
    Set-ItemProperty -LiteralPath $mm -Name 'SystemResponsiveness' -Value 10
    $d = Compare-IemFingerprint -Baseline $f1 -Current (Get-IemReaperFingerprint -ProfilePath $pp)
    Assert ($d.Count -eq 1 -and $d[0].key -eq 'mmcss.SystemResponsiveness') 'fingerprint-names-a-change'

    # Inventory: read only, serializable, no command lines or image paths.
    $inv = Get-IemInventory -ProfilePath $pp
    $json = $inv | ConvertTo-Json -Depth 8
    Assert ($json.Length -gt 1000 -and $json -notmatch 'PathName|CommandLine') 'inventory-serializes-without-command-lines'

    # Measurement helpers that need no xperf.
    $a = New-IemTraceArguments -Dir 'C:\t' -CSwitch -CircularMB 1024
    Assert (($a -join ' ') -eq '-on PROC_THREAD+LOADER+DPC+INTERRUPT+CSWITCH+DISPATCHER -BufferSize 1024 -MinBuffers 256 -MaxBuffers 1024 -FileMode Circular -MaxFile 1024 -f C:\t\kernel.etl -start IemMarkers -on 3b6c1e0a-5d2f-4c8e-9a71-0e4f2d9b8c11 -f C:\t\markers.etl') 'trace-arguments'
    $l = ConvertFrom-IemLoggers -Text "Logger Name           : NT Kernel Logger`r`nLogger Mode Settings (11)`r`nLogger Name           : IemMarkers`r`n"
    Assert ($l.Count -eq 2 -and $l[0] -eq 'NT Kernel Logger' -and $l[1] -eq 'IemMarkers') 'loggers-parse'
    # The near-glitch export works on any trace file (lane F2's cut-aware export):
    # trace.etl -> near.txt, <base>.etl -> <base>.near.txt, each through its own
    # dumper temp file (Invoke-IemDpcIsr's naming); the filter keeps the dumper's
    # header, the DPC/ISR/context-switch rows and the glitch markers.
    Assert ((Get-Command Export-IemNearGlitch).Parameters['Name'].ParameterType -eq [string]) 'near-glitch-export-takes-a-trace-name'
    $nf = Get-IemNearGlitchFiles -Dir 'C:\t' -Name 'cut-2.etl'
    Assert ($nf.input -eq 'C:\t\cut-2.etl' -and $nf.dump -eq 'C:\t\cut-2.dumper.txt' -and $nf.near -eq 'C:\t\cut-2.near.txt') 'near-glitch-files-for-a-cut'
    $nf = Get-IemNearGlitchFiles -Dir 'C:\t'
    Assert ($nf.input -eq 'C:\t\trace.etl' -and $nf.dump -eq 'C:\t\dumper.txt' -and $nf.near -eq 'C:\t\near.txt') 'near-glitch-files-for-the-trace'
    $ndump = Join-Path $dir 'cut-2.dumper.txt'; $nnear = Join-Path $dir 'cut-2.near.txt'
    [IO.File]::WriteAllText($ndump, "BeginHeader`r`nh1`r`nEndHeader`r`nDPC, 1, x`r`nDiskRead, 2, y`r`nInterrupt, 3, z`r`nMark, 4, iemmixer-glitch 5`r`n")
    $sn = Select-IemNearGlitch -Dump $ndump -Near $nnear
    $nl = @(Get-Content -LiteralPath $nnear)
    Assert ($sn -eq 6 -and ($nl -join '|') -eq 'BeginHeader|h1|EndHeader|DPC, 1, x|Interrupt, 3, z|Mark, 4, iemmixer-glitch 5') 'near-glitch-keeps-header-dpc-isr-and-markers'
    # A native program's stderr is output: its exit code alone decides (A11). A
    # stand-in program writes a line to each stream. (It is tested through
    # Invoke-IemNative: Invoke-IemXperf now refuses an unsigned stand-in, m4.)
    $fx = Join-Path $dir 'fake-native.cmd'
    [IO.File]::WriteAllText($fx, "@echo off`r`necho out-line`r`necho err-line 1>&2`r`nexit /b 0`r`n")
    $no = Invoke-IemNative -FilePath $fx -Arguments @('-Loggers')
    $nt = (@($no.out) -join ' ')
    Assert ($no.code -eq 0 -and $nt -match 'out-line' -and $nt -match 'err-line') 'native-stderr-with-exit-0-is-output'
    ThrowsLike { Invoke-IemXperf -Xperf $fx -Arguments @('-Loggers') } '*signature*' 'xperf-refuses-an-unsigned-binary'
    [IO.File]::WriteAllText($fx, "@echo off`r`necho bad-line 1>&2`r`nexit /b 3`r`n")
    $nb = Invoke-IemNative -FilePath $fx
    Assert ($nb.code -eq 3 -and ((@($nb.out) -join ' ') -match 'bad-line')) 'native-reports-a-nonzero-exit'
    # xperf runs only when Microsoft signed it and it is new enough, checked once per
    # process (m4); PING.EXE stands in for a signed binary.
    $xo = (Invoke-IemXperf -Xperf $ping -Arguments @('-n', '1', '127.0.0.1')) -join ' '
    Assert ($xo -match '127\.0\.0\.1') 'xperf-runs-a-signed-binary'
    ThrowsLike { Invoke-IemXperf -Xperf $ping -Arguments @('-n', 'x', '127.0.0.1') } '*(exit *' 'xperf-a-nonzero-exit-throws'
    # The stop without -Merge is the pre-emption path ("ide event") and always works
    # (review 3.6): it runs xperf unchecked (it was checked when the trace started),
    # and when xperf cannot run at all it stops the sessions with logman.
    $fl = Join-Path $dir 'fake-xperf-no-sessions.cmd'
    [IO.File]::WriteAllText($fl, "@echo off`r`nexit /b 0`r`n")
    $s1 = $null; $se = $null
    try { $s1 = Stop-IemTrace -Xperf $fl -Dir $dir } catch { $se = "$_" }
    Assert ($null -eq $se -and $s1.via -eq 'xperf' -and @($s1.stopped).Count -eq 0) "trace-stop-runs-xperf-unchecked ($se)"
    $lm = Invoke-IemNative -FilePath 'logman.exe' -Arguments @('start', 'IemMarkers', '-p', '{3b6c1e0a-5d2f-4c8e-9a71-0e4f2d9b8c11}', '-o', (Join-Path $dir 'markers-test.etl'), '-ets')
    Assert ($lm.code -eq 0) "marker-session-starts ($($lm.out -join ' '))"
    $s2 = $null; $se = $null
    try { $s2 = Stop-IemTrace -Xperf (Join-Path $dir 'no-xperf.exe') -Dir $dir } catch { $se = "$_" }
    Assert ($null -eq $se -and $s2.via -eq 'logman' -and @($s2.stopped) -contains 'IemMarkers') "trace-stop-falls-back-to-logman ($se)"
    $lq = Invoke-IemNative -FilePath 'logman.exe' -Arguments @('query', '-ets')
    Assert ($lq.code -eq 0 -and -not (@($lq.out) -match '^\s*IemMarkers\s')) 'trace-stop-leaves-no-marker-session'
    # An existing xperf counts as installed only when Microsoft signed it and its
    # version is new enough (A12); PING.EXE stands in for a signed binary.
    $fakeX = Join-Path $dir 'xperf.exe'
    [IO.File]::WriteAllText($fakeX, 'not a signed binary')
    $noSetup = Join-Path $dir 'no-adksetup.exe'
    ThrowsLike { Install-IemWpt -Setup $noSetup -Xperf $fakeX } '*signature*' 'wpt-refuses-an-unsigned-xperf'
    $wo = Install-IemWpt -Setup $noSetup -Xperf $ping
    Assert ($wo.installed -eq 'already' -and "$($wo.version)" -like '10.*') 'wpt-accepts-a-microsoft-signed-binary'
    ThrowsLike { Install-IemWpt -Setup $noSetup -Xperf $ping -MinVersion '99.0' } '*older than 99.0*' 'wpt-refuses-an-older-version'
    $c = Get-IemCpuSample
    Assert ($c.cpus.Count -ge 1 -and $c.cpus[0].t100ns -gt 0) 'cpu-sample-reads-raw-counters'
    [void](Get-IemSystemEvents -Since ((Get-Date).AddHours(-1).ToUniversalTime().ToString('o')))
    Write-Host 'ok  system-events-read'
    # "No events found" is an empty result; any other query error throws, never
    # reads as zero WHEA/driver-reset/power events (A10).
    $none = Get-IemSystemEvents -Since ((Get-Date).AddDays(1).ToUniversalTime().ToString('o'))
    Assert (@($none).Count -eq 0) 'system-events-none-found-is-empty'
    ThrowsLike { Get-IemSystemEvents -Since ((Get-Date).AddHours(-1).ToUniversalTime().ToString('o')) -LogName "iemmixer-no-such-log-$id" } "*iemmixer-no-such-log-$id*" 'system-events-a-failing-query-throws'
} finally {
    try { [void](Exit-IemTuningMode -ProfilePath $pp) } catch { Write-Host "cleanup exit: $_" }
    foreach ($t in 2, 3) { try { [void](Undo-IemTuning -ProfilePath $pp -Tier $t) } catch { Write-Host "cleanup undo: $_" } }
    if ([IemPower]::Active() -ne $activeBefore) { [IemPower]::Activate($activeBefore) }
    foreach ($g in $testPlan, $foreignPlan, $halfPlan) { if (@(& powercfg.exe /list) -match $g) { & powercfg.exe /delete $g | Out-Null } }
    Unregister-ScheduledTask -TaskPath $taskPath -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    if (@((Get-MpPreference).ExclusionPath) -contains $dir) { Remove-MpPreference -ExclusionPath $dir }
    try { [void](Invoke-IemNative -FilePath 'logman.exe' -Arguments @('stop', 'IemMarkers', '-ets')) } catch { Write-Host "cleanup logman: $_" }
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host 'Test-IemTuning: all passed'
