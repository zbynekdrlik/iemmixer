#Requires -Version 5.1
# Self-test of the S1c tuning modules on Windows PowerShell 5.1 (CI job asio-spike,
# an ephemeral administrator runner): real backends — registry values under an
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
function Rows($rows, $action) { ,@($rows | Where-Object { $_.action -eq $action }) }
# Read-IemJournal is exported (every *-Iem* function is); the test reads the flag the module wrote.
function Read-IemJournalState($profilePath) { $p = Read-IemProfile -Path $profilePath; (Read-IemJournal -Path $p.journal).entered }

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
New-ItemProperty -LiteralPath $nic -Name 'PowerSaving' -PropertyType String -Value '1' | Out-Null
$mm = "$root\HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile"
New-Item -Path "$mm\Tasks\Pro Audio" -Force | Out-Null
New-ItemProperty -LiteralPath $mm -Name 'SystemResponsiveness' -PropertyType DWord -Value 0 | Out-Null
$ping = "$env:SystemRoot\System32\PING.EXE"
$child = Start-Process -FilePath $ping -ArgumentList '-n', '240', '127.0.0.1' -PassThru -WindowStyle Hidden

function New-TestProfile([string]$Hwid) {
    $p = [ordered]@{
        version = 1; journal = (Join-Path $dir 'journal.json'); registry_root = $root
        layout = [ordered]@{ housekeeping = @(0); card = @(0); nic = @(0); audio = @(0) }
        plan = [ordered]@{ guid = $testPlan; source = $activeBefore }
        governor = 'W32Time'; placement = @('PING'); services_disable = @('Spooler'); services_mode = @()
        updates = [ordered]@{ services = @(); tasks = @() }
        maintenance = [ordered]@{ off = $true; tasks = @("$taskPath$taskName", '\iemmixer-test\no-such-task') }
        defender = [ordered]@{ paths = @($dir); processes = @() }
        devices = @([ordered]@{ id = 'card'; instance = 'PCI\VEN_TEST&DEV_0001\0'; hwid = $Hwid; lps = @(0, 2); enabled = $true })
        nic = [ordered]@{ adapter = 'unused'; key = 'HKLM:\NIC'; properties = [ordered]@{ PowerSaving = '0' }; rss = [ordered]@{ base = 4; max = 5 }; pnp_capabilities = 24 }
        fingerprint = [ordered]@{ files = @(); keys = @() }
    }
    $path = Join-Path $dir "profile-$([guid]::NewGuid().ToString('N')).json"
    [IO.File]::WriteAllText($path, ($p | ConvertTo-Json -Depth 6))
    return $path
}
$pp = New-TestProfile 'PCI\VEN_TEST&DEV_0001'
$maint = "$root\HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Schedule\Maintenance"

try {
    # Tier 2: services, a task (plus an absent one), the maintenance switch, a Defender exclusion.
    $r = Invoke-IemTuningApply -ProfilePath $pp -Tier 2
    Assert ((Rows $r 'failed').Count -eq 0) "tier2-apply-has-no-failure ($(@(Rows $r 'failed') | ForEach-Object { $_.error }))"
    Assert ((Get-Service Spooler).Status -eq 'Stopped' -and (Get-Item 'HKLM:\SYSTEM\CurrentControlSet\Services\Spooler').GetValue('Start') -eq 4) 'tier2-service-disabled-and-stopped'
    Assert ("$((Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName).State)" -eq 'Disabled') 'tier2-task-disabled'
    Assert ((Rows $r 'absent').Count -eq 1) 'tier2-a-missing-task-is-absent-not-an-error'
    Assert ((Get-Item -LiteralPath $maint).GetValue('MaintenanceDisabled') -eq 1) 'tier2-maintenance-off'
    Assert (@((Get-MpPreference).ExclusionPath) -contains $dir) 'tier2-defender-exclusion'
    $again = Invoke-IemTuningApply -ProfilePath $pp -Tier 2
    Assert ((Rows $again 'written').Count -eq 0 -and (Rows $again 'failed').Count -eq 0) 'tier2-apply-is-idempotent'
    $u = Undo-IemTuning -ProfilePath $pp -Tier 2
    Assert ((Rows $u 'failed').Count -eq 0) 'tier2-undo-has-no-failure'
    Assert ((Get-Service Spooler).Status -eq 'Running' -and (Get-Item 'HKLM:\SYSTEM\CurrentControlSet\Services\Spooler').GetValue('Start') -eq $spoolStart) 'tier2-undo-restores-the-original'
    Assert ("$((Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName).State)" -ne 'Disabled') 'tier2-undo-enables-the-task'
    Assert ($null -eq (Get-Item -LiteralPath $maint).GetValue('MaintenanceDisabled', $null)) 'tier2-undo-deletes-absent-values'
    Assert (-not (@((Get-MpPreference).ExclusionPath) -contains $dir)) 'tier2-undo-removes-the-exclusion'

    # Tier 3: affinity policy under the device's key, NIC values; pending until a reboot.
    $bad = New-TestProfile 'PCI\VEN_OTHER'
    Throws { Invoke-IemTuningApply -ProfilePath $bad -Tier 3 -Only @('irq') } 'tier3-refuses-a-mismatched-device'
    Assert (-not (Test-Path -LiteralPath "$enum\Device Parameters")) 'tier3-refusal-writes-nothing'
    $r3 = Invoke-IemTuningApply -ProfilePath $pp -Tier 3
    Assert ((Rows $r3 'failed').Count -eq 0) 'tier3-apply-has-no-failure'
    $ap = Get-Item -LiteralPath "$enum\Device Parameters\Interrupt Management\Affinity Policy"
    Assert ($ap.GetValue('DevicePolicy') -eq 4 -and $ap.GetValue('AssignmentSetOverride') -eq 5) 'tier3-affinity-policy-and-mask'
    Assert ((Get-Item -LiteralPath $nic).GetValue('PowerSaving') -eq '0' -and (Get-Item -LiteralPath $nic).GetValue('*RssBaseProcNumber') -eq '4') 'tier3-nic-values'
    $st = Get-IemTuningState -ProfilePath $pp
    Assert (@($st.items | Where-Object { $_.tier -eq 3 -and -not $_.pending }).Count -eq 0) 'tier3-items-are-pending-until-a-reboot'
    $u3 = Undo-IemTuning -ProfilePath $pp -Tier 3
    Assert ((Rows $u3 'failed').Count -eq 0) 'tier3-undo-has-no-failure'
    Assert ($null -eq $ap.GetValue('DevicePolicy', $null) -and (Get-Item -LiteralPath $nic).GetValue('PowerSaving') -eq '1') 'tier3-undo-deletes-absent-values'
    $st = Get-IemTuningState -ProfilePath $pp
    Assert (@($st.items | Where-Object { $_.key -eq 'irq:card:policy' -and $_.revert_pending }).Count -eq 1) 'tier3-undo-is-pending-until-a-reboot'

    # Mode levers: plan (C1 only), governor stand-in, placement.
    $e = Enter-IemTuningMode -ProfilePath $pp -Only @('plan', 'governor', 'placement') -Idle 'c1'
    Assert ((Rows $e 'failed').Count -eq 0) "enter-has-no-failure ($(@(Rows $e 'failed') | ForEach-Object { $_.key + ': ' + $_.error }))"
    Assert ([IemPower]::Active() -eq $testPlan) 'enter-activates-the-plan'
    Assert ([IemPower]::Read($testPlan, '54533251-82be-4824-96c1-47b60b740d00', '9943e905-9a30-4ec1-9b99-44dd3b76f7a2') -eq 1) 'enter-limits-idle-to-c1'
    Assert ([IemPower]::Read($testPlan, '54533251-82be-4824-96c1-47b60b740d00', '893dee8e-2bef-41e0-89c6-b55d0929964c') -eq 100) 'enter-sets-processor-min-100'
    Assert ((Get-Service W32Time).Status -eq 'Stopped') 'enter-pauses-the-governor'
    $hk = [IemCpuSets]::Map()[0]
    Assert ((@([IemCpuSets]::Get($child.Id)) -join ',') -eq "$hk") 'enter-places-the-process'
    $e2 = Enter-IemTuningMode -ProfilePath $pp -Only @('plan', 'governor', 'placement') -Idle 'c1'
    Assert ((Rows $e2 'written').Count -eq 0) 'enter-is-idempotent'
    # A new session restores from the journal alone.
    Remove-Module IemMeasure, IemTuning
    Import-Module (Join-Path $here 'IemMeasure.psm1') -Force
    $x = Exit-IemTuningMode -ProfilePath $pp
    Assert ([IemPower]::Active() -eq $activeBefore) 'exit-from-journal-in-a-new-session'
    Assert (-not (@(& powercfg.exe /list) -match $testPlan)) 'exit-deletes-the-plan'
    Assert ((Get-Service W32Time).Status -eq 'Running') 'exit-restarts-the-governor'
    Assert ((@([IemCpuSets]::Get($child.Id)) -join ',') -eq '') 'exit-clears-the-placement'
    Assert (-not (Read-IemJournalState $pp)) 'exit-clears-entered'
    Assert ((Exit-IemTuningMode -ProfilePath $pp).Count -eq 0) 'exit-twice-is-harmless'

    # A placed process that ended is skipped; a reused pid is refused.
    $short = Start-Process -FilePath $ping -ArgumentList '-n', '5', '127.0.0.1' -PassThru -WindowStyle Hidden
    [void](Enter-IemTuningMode -ProfilePath $pp -Only @('placement'))
    $short.WaitForExit(10000) | Out-Null
    $x = Exit-IemTuningMode -ProfilePath $pp
    Assert ((Rows $x 'gone').Count -ge 1) 'exit-skips-a-process-that-ended'
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
    $c = Get-IemCpuSample
    Assert ($c.cpus.Count -ge 1 -and $c.cpus[0].t100ns -gt 0) 'cpu-sample-reads-raw-counters'
    $ps = Get-IemPollSample -ProfilePath $pp
    Assert ($ps.plan -eq $activeBefore -and $ps.governor -eq 'Running') 'poll-sample-reads-the-sentinels'
    [void](Get-IemSystemEvents -Since ((Get-Date).AddHours(-1).ToUniversalTime().ToString('o')))
    Write-Host 'ok  system-events-read'
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
