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
function Throws([scriptblock]$b, $what) { $t = $false; try { & $b } catch { $t = $true }; Assert $t $what }
# Callers wrap this in @(...) so .Count and a ForEach pipe are array-safe under
# StrictMode on PS 5.1 (a bare (Rows ...) would be $null for 0 matches; a ,@()
# return would make @(Rows ...) iterate once over an empty array; S1c CI).
function Rows($rows, $action) { @($rows | Where-Object { $_.action -eq $action }) }
# Read-IemJournal is exported (every *-Iem* function is); the test reads the flag the module wrote.
function Read-IemJournalState($profilePath) { $p = Read-IemProfile -Path $profilePath; (Read-IemJournal -Path $p.journal).entered }
function Read-JournalVersion($profilePath) { $p = Read-IemProfile -Path $profilePath; (Read-IemJournal -Path $p.journal).version }
# Sets the boot of every global journal entry, as if its item had been written in that boot.
function Set-JournalBoot($profilePath, [string]$boot) {
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
$taskPath = '\iemmixer-test\'; $taskName = "t-$id"
Register-ScheduledTask -TaskPath $taskPath -TaskName $taskName -Action (New-ScheduledTaskAction -Execute 'cmd.exe' -Argument '/c exit 0') | Out-Null
$enum = "$root\HKLM\SYSTEM\CurrentControlSet\Enum\PCI\VEN_TEST&DEV_0001\0"
New-Item -Path $enum -Force | Out-Null
New-ItemProperty -LiteralPath $enum -Name 'HardwareID' -PropertyType MultiString -Value @('PCI\VEN_TEST&DEV_0001&SUBSYS_1', 'PCI\VEN_TEST&DEV_0001') | Out-Null
$nic = "$root\HKLM\NIC"
New-Item -Path $nic -Force | Out-Null
# The NIC driver key names the hardware id its driver matched (A8).
New-ItemProperty -LiteralPath $nic -Name 'MatchingDeviceId' -PropertyType String -Value 'pci\ven_test&dev_0002' | Out-Null
New-ItemProperty -LiteralPath $nic -Name 'PowerSaving' -PropertyType String -Value '1' | Out-Null
# A value whose name ends like the NDIS keyword *EEE: undoing *EEE must leave it (A2).
New-ItemProperty -LiteralPath $nic -Name 'AdvancedEEE' -PropertyType String -Value '1' | Out-Null
# Values of other kinds than the items write: undo restores their own kind and data (A1).
New-ItemProperty -LiteralPath $nic -Name 'IemDword' -PropertyType DWord -Value 1 | Out-Null
New-ItemProperty -LiteralPath $nic -Name 'IemExpand' -PropertyType ExpandString -Value '%SystemRoot%\iem' | Out-Null
$mm = "$root\HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile"
New-Item -Path "$mm\Tasks\Pro Audio" -Force | Out-Null
New-ItemProperty -LiteralPath $mm -Name 'SystemResponsiveness' -PropertyType DWord -Value 0 | Out-Null
$ping = "$env:SystemRoot\System32\PING.EXE"
$child = Start-Process -FilePath $ping -ArgumentList '-n', '240', '127.0.0.1' -PassThru -WindowStyle Hidden

function New-TestNic([string]$Hwid, [string]$Adapter = '') {
    # The test NIC: its driver key HKLM:\NIC under the test root, or with -Adapter found by adapter name.
    $n = [ordered]@{ adapter = 'unused'; key = 'HKLM:\NIC'; hwid = $Hwid; properties = [ordered]@{ PowerSaving = '0'; '*EEE' = '0'; IemDword = '0'; IemExpand = 'plain' }
                     rss = [ordered]@{ base = 4; max = 5 }; pnp_capabilities = 24 }
    if ($Adapter) { $n.adapter = $Adapter; $n.Remove('key') }
    return $n
}

function New-TestProfile([string]$Hwid, [hashtable]$Set = @{}) {
    $p = [ordered]@{
        version = 1; journal = (Join-Path $dir 'journal.json'); registry_root = $root
        layout = [ordered]@{ housekeeping = @(0); card = @(0); nic = @(0); audio = @(0) }
        plan = [ordered]@{ guid = $testPlan; source = $activeBefore }
        governor = 'W32Time'; placement = @('PING'); services_disable = @('Spooler'); services_mode = @()
        updates = [ordered]@{ services = @(); tasks = @() }
        maintenance = [ordered]@{ off = $true; tasks = @("$taskPath$taskName", '\iemmixer-test\no-such-task') }
        defender = [ordered]@{ paths = @($dir); processes = @() }
        devices = @([ordered]@{ id = 'card'; instance = 'PCI\VEN_TEST&DEV_0001\0'; hwid = $Hwid; lps = @(0, 2); enabled = $true })
        nic = (New-TestNic 'PCI\VEN_TEST&DEV_0002')
        fingerprint = [ordered]@{ files = @(); keys = @() }
    }
    foreach ($k in @($Set.Keys)) { $p[$k] = $Set[$k] }
    $path = Join-Path $dir "profile-$([guid]::NewGuid().ToString('N')).json"
    [IO.File]::WriteAllText($path, ($p | ConvertTo-Json -Depth 6))
    return $path
}
$pp = New-TestProfile 'PCI\VEN_TEST&DEV_0001'
$maint = "$root\HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Schedule\Maintenance"

try {
    # Journal file: a flushed temp file swapped in; a stop in between leaves the
    # journal missing or empty next to a complete .tmp, which the read uses (A14).
    $jp = Join-Path (Join-Path $dir 'journal-file') 'journal.json'
    $jj = @{ schema = 1; version = 7; entered = $true; global = @{}; mode = @{}; reverted = @{}; order = @{ global = @(); mode = @() } }
    Write-IemJournal -Path $jp -Journal $jj
    Assert ((Read-IemJournal -Path $jp).version -eq 7 -and -not (Test-Path -LiteralPath "$jp.tmp")) 'journal-write-leaves-no-temp-file'
    $jj.version = 8
    Write-IemJournal -Path $jp -Journal $jj
    Assert ((Read-IemJournal -Path $jp).version -eq 8) 'journal-write-replaces-the-journal'
    Move-Item -LiteralPath $jp -Destination "$jp.tmp"
    Assert ((Read-IemJournal -Path $jp).version -eq 8) 'journal-read-falls-back-to-a-complete-temp-file'
    [IO.File]::WriteAllText($jp, '')
    Assert ((Read-IemJournal -Path $jp).version -eq 8) 'journal-read-falls-back-when-the-journal-is-empty'
    Remove-Item -LiteralPath "$jp.tmp"
    Throws { Read-IemJournal -Path $jp } 'journal-read-refuses-an-empty-journal-without-a-temp-file'

    # -Only names groups of the tier: a typo is an error, never an empty apply (A9).
    Throws { Invoke-IemTuningApply -ProfilePath $pp -Tier 2 -Only @('servics') } 'apply-refuses-an-unknown-group'
    Throws { Undo-IemTuning -ProfilePath $pp -Tier 2 -Only @('servics') } 'undo-refuses-an-unknown-group'
    # The profile version is stamped only after a complete apply without a failure (A9).
    $rm = Invoke-IemTuningApply -ProfilePath $pp -Tier 2 -Only @('maintenance')
    Assert (@(Rows $rm 'failed').Count -eq 0 -and (Read-JournalVersion $pp) -eq 0) 'apply-partial-does-not-stamp-the-version'

    # Tier 2: services, a task (plus an absent one), the maintenance switch, a Defender exclusion.
    $r = Invoke-IemTuningApply -ProfilePath $pp -Tier 2
    Assert (@(Rows $r 'failed').Count -eq 0) "tier2-apply-has-no-failure ($(@(Rows $r 'failed') | ForEach-Object { $_.error }))"
    Assert ((Get-Service Spooler).Status -eq 'Stopped' -and (Get-Item 'HKLM:\SYSTEM\CurrentControlSet\Services\Spooler').GetValue('Start') -eq 4) 'tier2-service-disabled-and-stopped'
    Assert ("$((Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName).State)" -eq 'Disabled') 'tier2-task-disabled'
    Assert (@(Rows $r 'absent').Count -eq 1) 'tier2-a-missing-task-is-absent-not-an-error'
    Assert ((Get-Item -LiteralPath $maint).GetValue('MaintenanceDisabled') -eq 1) 'tier2-maintenance-off'
    Assert (@((Get-MpPreference).ExclusionPath) -contains $dir) 'tier2-defender-exclusion'
    Assert ((Read-JournalVersion $pp) -eq 1) 'apply-complete-stamps-the-version'
    $again = Invoke-IemTuningApply -ProfilePath $pp -Tier 2
    Assert (@(Rows $again 'written').Count -eq 0 -and @(Rows $again 'failed').Count -eq 0) 'tier2-apply-is-idempotent'
    # An exclusion Defender cannot hold (an empty path) fails its row: version 2 is not stamped.
    $pf = New-TestProfile 'PCI\VEN_TEST&DEV_0001' @{ version = 2; defender = [ordered]@{ paths = @($dir, ''); processes = @() } }
    $rf = Invoke-IemTuningApply -ProfilePath $pf -Tier 2
    Assert (@(Rows $rf 'failed').Count -eq 1 -and (Read-JournalVersion $pp) -eq 1) 'apply-with-a-failure-does-not-stamp-the-version'
    $u = Undo-IemTuning -ProfilePath $pp -Tier 2
    Assert (@(Rows $u 'failed').Count -eq 0) 'tier2-undo-has-no-failure'
    Assert ((Get-Service Spooler).Status -eq 'Running' -and (Get-Item 'HKLM:\SYSTEM\CurrentControlSet\Services\Spooler').GetValue('Start') -eq $spoolStart) 'tier2-undo-restores-the-original'
    Assert ("$((Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName).State)" -ne 'Disabled') 'tier2-undo-enables-the-task'
    Assert ($null -eq (Get-Item -LiteralPath $maint).GetValue('MaintenanceDisabled', $null)) 'tier2-undo-deletes-absent-values'
    Assert (-not (@((Get-MpPreference).ExclusionPath) -contains $dir)) 'tier2-undo-removes-the-exclusion'

    # Tier 3: affinity policy under the device's key, NIC values; pending until a reboot.
    $bad = New-TestProfile 'PCI\VEN_OTHER'
    Throws { Invoke-IemTuningApply -ProfilePath $bad -Tier 3 -Only @('irq') } 'tier3-refuses-a-mismatched-device'
    Assert (-not (Test-Path -LiteralPath "$enum\Device Parameters")) 'tier3-refusal-writes-nothing'
    # The NIC driver key is checked against the profile's hardware id before any write,
    # and found under registry_root also by adapter name (design note 7, A8).
    $badNic = New-TestProfile 'PCI\VEN_TEST&DEV_0001' @{ nic = (New-TestNic 'PCI\VEN_OTHER') }
    Throws { Invoke-IemTuningApply -ProfilePath $badNic -Tier 3 -Only @('nic') } 'tier3-refuses-a-mismatched-nic'
    $nk = Get-Item -LiteralPath $nic
    Assert ($nk.GetValue('PowerSaving') -eq '1' -and $null -eq $nk.GetValue('*RssBaseProcNumber', $null)) 'tier3-nic-refusal-writes-nothing'
    $an = @(Get-NetAdapter)[0]
    $cls = "$root\HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e972-e325-11ce-bfc1-08002be10318}\0000"
    New-Item -Path $cls -Force | Out-Null
    New-ItemProperty -LiteralPath $cls -Name 'NetCfgInstanceId' -PropertyType String -Value "$($an.InterfaceGuid)" | Out-Null
    New-ItemProperty -LiteralPath $cls -Name 'MatchingDeviceId' -PropertyType String -Value 'pci\ven_test&dev_0002' | Out-Null
    $byName = Read-IemProfile -Path (New-TestProfile 'PCI\VEN_TEST&DEV_0001' @{ nic = (New-TestNic 'PCI\VEN_TEST&DEV_0002' $an.Name) })
    Assert ("$(Get-IemNicKey -Profile $byName)" -like "*iemmixer-tuning-test-$id*") 'tier3-nic-by-adapter-name-stays-under-registry-root'
    # R1 applies only while the card already uses MSI (design note 6.4 R1): otherwise
    # it is skipped with its reason, and nothing is written (A7).
    $rs = Invoke-IemTuningApply -ProfilePath $pp -Tier 3 -Only @('irq')
    $sk = @(Rows $rs 'skipped')
    Assert ($sk.Count -eq 1 -and "$($sk[0].value)" -like '*line-based*' -and @(Rows $rs 'written').Count -eq 0 -and -not (Test-Path -LiteralPath "$enum\Device Parameters")) 'tier3-skips-the-card-without-msi'
    $msiKey = "$enum\Device Parameters\Interrupt Management\MessageSignaledInterruptProperties"
    New-Item -Path $msiKey -Force | Out-Null
    New-ItemProperty -LiteralPath $msiKey -Name 'MSISupported' -PropertyType DWord -Value 1 | Out-Null
    # The card's mask exists as REG_BINARY (a KAFFINITY); the item writes a QWORD (A1).
    $apKey = "$enum\Device Parameters\Interrupt Management\Affinity Policy"
    New-Item -Path $apKey -Force | Out-Null
    New-ItemProperty -LiteralPath $apKey -Name 'AssignmentSetOverride' -PropertyType Binary -Value ([byte[]](4, 0, 0, 0, 0, 0, 0, 0)) | Out-Null
    $r3 = Invoke-IemTuningApply -ProfilePath $pp -Tier 3
    Assert (@(Rows $r3 'failed').Count -eq 0) 'tier3-apply-has-no-failure'
    $ap = Get-Item -LiteralPath "$enum\Device Parameters\Interrupt Management\Affinity Policy"
    Assert ($ap.GetValue('DevicePolicy') -eq 4 -and $ap.GetValue('AssignmentSetOverride') -eq 5) 'tier3-affinity-policy-and-mask'
    Assert ((Get-Item -LiteralPath $nic).GetValue('PowerSaving') -eq '0' -and (Get-Item -LiteralPath $nic).GetValue('*RssBaseProcNumber') -eq '4') 'tier3-nic-values'
    $st = Get-IemTuningState -ProfilePath $pp
    Assert (@($st.items | Where-Object { $_.tier -eq 3 -and -not $_.pending }).Count -eq 0) 'tier3-items-are-pending-until-a-reboot'
    # A clock step (time sync) moves LastBootUpTime; the boot stays the same (A13).
    $step = [datetime]::Parse((Get-IemBootTime), [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::RoundtripKind).ToUniversalTime().AddSeconds(30).ToString('o')
    Set-JournalBoot $pp $step
    $st = Get-IemTuningState -ProfilePath $pp
    Assert (@($st.items | Where-Object { $_.tier -eq 3 -and -not $_.pending }).Count -eq 0) 'tier3-pending-survives-a-clock-step'
    # Pending means written after the current boot: the boot of the latest write counts (A3).
    Set-JournalBoot $pp '2000-01-01T00:00:00.0000000Z'
    $st = Get-IemTuningState -ProfilePath $pp
    Assert (@($st.items | Where-Object { $_.tier -eq 3 -and $_.pending }).Count -eq 0) 'tier3-an-earlier-boot-is-not-pending'
    Set-ItemProperty -LiteralPath $nic -Name 'PowerSaving' -Value '1'
    $rw = Invoke-IemTuningApply -ProfilePath $pp -Tier 3 -Only @('nic')
    Assert (@(Rows $rw 'written').Count -eq 1 -and @(Rows $rw 'failed').Count -eq 0) 'tier3-reapply-rewrites-a-changed-value'
    $st = Get-IemTuningState -ProfilePath $pp
    $again3 = @($st.items | Where-Object { $_.pending } | ForEach-Object { $_.key })
    Assert ($again3.Count -eq 1 -and $again3[0] -eq 'nic:PowerSaving') 'tier3-a-rewrite-is-pending-again'
    $u3 = Undo-IemTuning -ProfilePath $pp -Tier 3
    Assert (@(Rows $u3 'failed').Count -eq 0) 'tier3-undo-has-no-failure'
    Assert ($null -eq $ap.GetValue('DevicePolicy', $null) -and (Get-Item -LiteralPath $nic).GetValue('PowerSaving') -eq '1') 'tier3-undo-deletes-absent-values'
    $nk = Get-Item -LiteralPath $nic
    Assert ($null -eq $nk.GetValue('*EEE', $null) -and $nk.GetValue('AdvancedEEE') -eq '1') 'tier3-undo-deletes-exactly-the-literal-value'
    $apv = Get-Item -LiteralPath $apKey
    Assert ("$($apv.GetValueKind('AssignmentSetOverride'))" -eq 'Binary' -and (@($apv.GetValue('AssignmentSetOverride') | ForEach-Object { $_.ToString('x2') }) -join '') -eq '0400000000000000') 'tier3-undo-restores-a-binary-value'
    Assert ("$($nk.GetValueKind('IemDword'))" -eq 'DWord' -and $nk.GetValue('IemDword') -eq 1) 'tier3-undo-restores-a-dword-value'
    Assert ("$($nk.GetValueKind('IemExpand'))" -eq 'ExpandString' -and $nk.GetValue('IemExpand', $null, 'DoNotExpandEnvironmentNames') -eq '%SystemRoot%\iem') 'tier3-undo-restores-an-expand-string'
    $st = Get-IemTuningState -ProfilePath $pp
    Assert (@($st.items | Where-Object { $_.key -eq 'irq:card:policy' -and $_.revert_pending }).Count -eq 1) 'tier3-undo-is-pending-until-a-reboot'

    # Mode levers: plan (C1 only), governor stand-in, placement.
    $e = Enter-IemTuningMode -ProfilePath $pp -Only @('plan', 'governor', 'placement') -Idle 'c1'
    Assert (@(Rows $e 'failed').Count -eq 0) "enter-has-no-failure ($(@(Rows $e 'failed') | ForEach-Object { $_.key + ': ' + $_.error }))"
    Assert ([IemPower]::Active() -eq $testPlan) 'enter-activates-the-plan'
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

    # A placed process that ended is skipped; a reused pid is refused.
    $short = Start-Process -FilePath $ping -ArgumentList '-n', '5', '127.0.0.1' -PassThru -WindowStyle Hidden
    [void](Enter-IemTuningMode -ProfilePath $pp -Only @('placement'))
    $short.WaitForExit(10000) | Out-Null
    $x = Exit-IemTuningMode -ProfilePath $pp
    Assert (@(Rows $x 'gone').Count -ge 1) 'exit-skips-a-process-that-ended'
    Throws { Set-IemValue -Item ([pscustomobject]@{ key = 'k'; kind = 'cpusets'; args = @{ pid = $child.Id; name = 'PING'; start = 1 } }) -Value '' } 'cpusets-refuse-a-reused-pid'

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
    # xperf's stderr is output: its exit code alone decides (A11). A stand-in xperf
    # writes a line to each stream.
    $fx = Join-Path $dir 'fake-xperf.cmd'
    [IO.File]::WriteAllText($fx, "@echo off`r`necho out-line`r`necho err-line 1>&2`r`nexit /b 0`r`n")
    $xo = (Invoke-IemXperf -Xperf $fx -Arguments @('-Loggers')) -join ' '
    Assert ($xo -match 'out-line' -and $xo -match 'err-line') 'xperf-stderr-with-exit-0-is-not-an-error'
    [IO.File]::WriteAllText($fx, "@echo off`r`necho bad-line 1>&2`r`nexit /b 3`r`n")
    Throws { Invoke-IemXperf -Xperf $fx -Arguments @('-Loggers') } 'xperf-a-nonzero-exit-throws'
    # An existing xperf counts as installed only when Microsoft signed it and its
    # version is new enough (A12); PING.EXE stands in for a signed binary.
    $fakeX = Join-Path $dir 'xperf.exe'
    [IO.File]::WriteAllText($fakeX, 'not a signed binary')
    $noSetup = Join-Path $dir 'no-adksetup.exe'
    Throws { Install-IemWpt -Setup $noSetup -Xperf $fakeX } 'wpt-refuses-an-unsigned-xperf'
    $wo = Install-IemWpt -Setup $noSetup -Xperf $ping
    Assert ($wo.installed -eq 'already' -and "$($wo.version)" -like '10.*') 'wpt-accepts-a-microsoft-signed-binary'
    Throws { Install-IemWpt -Setup $noSetup -Xperf $ping -MinVersion '99.0' } 'wpt-refuses-an-older-version'
    $c = Get-IemCpuSample
    Assert ($c.cpus.Count -ge 1 -and $c.cpus[0].t100ns -gt 0) 'cpu-sample-reads-raw-counters'
    $ps = Get-IemPollSample -ProfilePath $pp
    Assert ($ps.plan -eq $activeBefore -and $ps.governor -eq 'Running') 'poll-sample-reads-the-sentinels'
    [void](Get-IemSystemEvents -Since ((Get-Date).AddHours(-1).ToUniversalTime().ToString('o')))
    Write-Host 'ok  system-events-read'
    # "No events found" is an empty result; any other query error throws, never
    # reads as zero WHEA/driver-reset/power events (A10).
    $none = Get-IemSystemEvents -Since ((Get-Date).AddDays(1).ToUniversalTime().ToString('o'))
    Assert (@($none).Count -eq 0) 'system-events-none-found-is-empty'
    Throws { Get-IemSystemEvents -Since ((Get-Date).AddHours(-1).ToUniversalTime().ToString('o')) -LogName "iemmixer-no-such-log-$id" } 'system-events-a-failing-query-throws'
} finally {
    try { [void](Exit-IemTuningMode -ProfilePath $pp) } catch { Write-Host "cleanup exit: $_" }
    foreach ($t in 2, 3) { try { [void](Undo-IemTuning -ProfilePath $pp -Tier $t) } catch { Write-Host "cleanup undo: $_" } }
    if ([IemPower]::Active() -ne $activeBefore) { [IemPower]::Activate($activeBefore) }
    if (@(& powercfg.exe /list) -match $testPlan) { & powercfg.exe /delete $testPlan | Out-Null }
    Unregister-ScheduledTask -TaskPath $taskPath -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    if (@((Get-MpPreference).ExclusionPath) -contains $dir) { Remove-MpPreference -ExclusionPath $dir }
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host 'Test-IemTuning: all passed'
