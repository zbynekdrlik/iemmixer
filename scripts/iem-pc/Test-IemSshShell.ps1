#Requires -Version 5.1
# Self-test of IemSshShell.psm1 (#15, the admin-only OpenSSH default shell) on
# Windows PowerShell 5.1 (CI job windows, an ephemeral administrator runner).
# Every function runs against a TEST key (HKLM:\SOFTWARE\iemmixer-ssh-test-<id>,
# made admin-only as the real one must be), a test task folder
# \iemmixer-test-ssh-<id>\ and a temp elevated root: never
# HKLM:\SOFTWARE\OpenSSH and never a task of \iemmixer. The undo task is run
# once as registered (SYSTEM, its entry script and module copies). The probe
# iempc composes runs through cmd.exe exactly as sshd starts it, with and
# without /d, and iempc_sshshell.parse_probe judges both. Only its own test
# objects are removed.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
Import-Module (Join-Path $here 'IemSshShell.psm1') -Force
function Assert($cond, $what) { if (-not $cond) { throw "FAILED: $what" } ; Write-Host "ok  $what" }
function ErrorOf([scriptblock]$b) { try { & $b; return '' } catch { return "$_" } }

$id = [guid]::NewGuid().ToString('N').Substring(0, 8)
$base = Join-Path ([IO.Path]::GetTempPath()) ('iem-sshshell-' + $id)
$er = Join-Path $base 'elevated'
$sub = 'SOFTWARE\iemmixer-ssh-test-' + $id
$key = 'HKLM:\' + $sub
$folder = '\iemmixer-test-ssh-' + $id
$taskName = 'iemmixer-ssh-shell-undo'
$me = Resolve-IemUser
$cmd = Join-Path ([Environment]::GetFolderPath('System')) 'cmd.exe'
$common = @{ Key = $key; TaskFolder = $folder; ElevatedRoot = $er }
$dir = Join-Path $er 'ssh-shell'
$prior = Join-Path $dir 'prior.json'
$hklm = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
$names = @('DefaultShell', 'DefaultShellCommandOption', 'DefaultShellArguments')
$sidUsers = 'S-1-5-32-545'

function New-TestKey {
    # The test key, admin-only like HKLM\SOFTWARE\OpenSSH must be: owner
    # Administrators, Administrators and SYSTEM full, Users read; no values.
    if ($null -ne $hklm.OpenSubKey($sub)) { $hklm.DeleteSubKeyTree($sub) }
    $sec = New-Object System.Security.AccessControl.RegistrySecurity
    $sec.SetOwner((New-Object System.Security.Principal.SecurityIdentifier 'S-1-5-32-544'))
    $sec.SetAccessRuleProtection($true, $false)
    foreach ($a in @(@('S-1-5-32-544', 'FullControl'), @('S-1-5-18', 'FullControl'), @($sidUsers, 'ReadKey'))) {
        $sec.AddAccessRule((New-Object System.Security.AccessControl.RegistryAccessRule((New-Object System.Security.Principal.SecurityIdentifier $a[0]),
            [System.Security.AccessControl.RegistryRights]$a[1], 'ContainerInherit', 'None', 'Allow')))
    }
    $k = $hklm.CreateSubKey($sub, [Microsoft.Win32.RegistryKeyPermissionCheck]::ReadWriteSubTree, $sec)
    $k.Close()
}

function Set-Foreign {
    # A prior shell of another kind each: a string, an unexpanded expandable
    # string and a multi-string (Undo must write back exactly these).
    $k = $hklm.OpenSubKey($sub, $true)
    try {
        $k.SetValue('DefaultShell', 'C:\Tools\othershell.exe', [Microsoft.Win32.RegistryValueKind]::String)
        $k.SetValue('DefaultShellCommandOption', '-c %IEMTESTVAR%', [Microsoft.Win32.RegistryValueKind]::ExpandString)
        $k.SetValue('DefaultShellArguments', [string[]]@('-a', 'b c'), [Microsoft.Win32.RegistryValueKind]::MultiString)
    } finally { $k.Close() }
}

function Read-Values {
    # Each value as "<kind>:<data>" (unexpanded; a multi-string joined by |), or "absent".
    $k = $hklm.OpenSubKey($sub)
    try {
        $out = @()
        foreach ($n in $names) {
            if (@($k.GetValueNames()) -notcontains $n) { $out += 'absent'; continue }
            $d = $k.GetValue($n, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            if ($d -is [array]) { $d = @($d) -join '|' }
            $out += ('{0}:{1}' -f $k.GetValueKind($n), $d)
        }
        return ($out -join ' ; ')
    } finally { $k.Close() }
}

$ours = "String:$cmd ; String:/d /c ; String:/d"
$absent = 'absent ; absent ; absent'
$foreign = 'String:C:\Tools\othershell.exe ; ExpandString:-c %IEMTESTVAR% ; MultiString:-a|b c'

function SidOf([string]$Name) {
    # A task principal's UserId: Task Scheduler may give a name or the SID itself.
    if ($Name -cmatch '^S-1-[0-9-]+$') { return $Name }
    return (New-Object System.Security.Principal.NTAccount $Name).Translate([System.Security.Principal.SecurityIdentifier]).Value
}

function Get-UndoTask {
    $sch = Connect-IemScheduler
    return (Get-IemRegisteredTask -Scheduler $sch -Folder $folder -Name $taskName)
}

function Wait-UndoTaskGone([int]$Seconds) {
    # The undo task removes itself once it restored; never ended by force.
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while ($clock.Elapsed.TotalSeconds -lt $Seconds) {
        if ($null -eq (Get-UndoTask) -and -not (Test-Path -LiteralPath $prior)) { return $true }
        Start-Sleep -Milliseconds 500
    }
    return $false
}

$compose = 'import sys; sys.path.insert(0, sys.argv[1]); import iempc, iempc_sshshell; print(iempc.module_script(iempc_sshshell.PROBE))'
$remoteOf = 'import sys; sys.path.insert(0, sys.argv[1]); import iempc; print(iempc.elevated_ps().REMOTE)'
# No double quote in a python -c line: Windows PowerShell 5.1 passes it to a native program unescaped.
$parse = 'import json, sys; sys.path.insert(0, sys.argv[1]); import iempc, iempc_sshshell; print(json.dumps(iempc_sshshell.parse_probe(iempc, json.load(open(sys.argv[2], encoding=''utf-8'')))))'

function Invoke-ThroughCmd([string]$Option) {
    # The probe as a fresh ssh session runs it: sshd starts the shell as
    # "<shell>" <option> "<command>" (the command iempc's ssh_cmd sends) and
    # writes the composed script to its stdin. Returns the probe's answer r.
    $script = (@(& python -c $compose $here) -join "`n") + "`n"
    if ($LASTEXITCODE -ne 0) { throw "python exited $LASTEXITCODE" }
    $remote = (@(& python -c $remoteOf $here) -join '').Trim()
    if ($LASTEXITCODE -ne 0) { throw "python exited $LASTEXITCODE" }
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $cmd
    $psi.Arguments = $Option + ' "' + $remote + '"'
    $psi.UseShellExecute = $false
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $p = [Diagnostics.Process]::Start($psi)
    $errRead = $p.StandardError.ReadToEndAsync()
    $p.StandardInput.Write($script)
    $p.StandardInput.Close()
    $out = $p.StandardOutput.ReadToEnd()
    if (-not $p.WaitForExit(120000)) { throw 'the probe still runs after 120 s (left running, never force-ended)' }
    $last = @($out -split "`r?`n" | Where-Object { $_.Trim() })[-1]
    $doc = $last | ConvertFrom-Json
    if ((Get-IemProp $doc 'ok') -ne $true) { throw ('the probe failed: ' + (Get-IemProp $doc 'error') + ' ' + $errRead.Result) }
    return $doc.r
}

function Test-Parse($R) {
    # iempc_sshshell.parse_probe on the probe's answer: exit 0 = accepted.
    $f = Join-Path $base ('probe-' + [guid]::NewGuid().ToString('N') + '.json')
    [IO.File]::WriteAllText($f, (ConvertTo-Json -InputObject $R -Compress), (New-Object System.Text.UTF8Encoding $false))
    # A refusal is a traceback on stderr: read it as text (Stop would throw on its first line).
    $eap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $said = @(& python -c $parse $here $f 2>&1 | ForEach-Object { "$_" }) -join ' '
        $code = $LASTEXITCODE
    } finally { $ErrorActionPreference = $eap }
    return [pscustomobject]@{ ok = ($code -eq 0); said = $said }
}

try {
    New-Item -ItemType Directory -Force -Path $base | Out-Null

    # ---- the probe, through cmd.exe as sshd starts it ----
    $withD = Invoke-ThroughCmd '/d /c'
    Assert ([string]$withD.exe -eq $cmd) "probe-reads-the-shell-it-ran-under ($($withD.exe))"
    $judged = Test-Parse $withD
    Assert $judged.ok "probe-a-shell-run-with-d-before-the-command-passes ($($withD.line); $($judged.said))"
    $withoutD = Invoke-ThroughCmd '/c'
    $judged = Test-Parse $withoutD
    Assert (-not $judged.ok -and $judged.said -like '*did not run with /d*') "probe-sshd-s-own-fallback-without-d-is-refused ($($withoutD.line); $($judged.said))"

    # ---- refusals before anything is written ----
    New-TestKey
    $e = ErrorOf { Set-IemSshShell @common }
    Assert ($e -like '*elevated root*refused*' -and (Read-Values) -eq $absent -and $null -eq (Get-UndoTask)) "set-refuses-an-elevated-root-that-is-not-admin-only ($e)"
    Install-IemElevatedFolder -Path $er -UserSid $me.sid
    $k = $hklm.OpenSubKey($sub, [Microsoft.Win32.RegistryKeyPermissionCheck]::ReadWriteSubTree, [System.Security.AccessControl.RegistryRights]::ChangePermissions -bor [System.Security.AccessControl.RegistryRights]::ReadPermissions)
    $acl = $k.GetAccessControl()
    $acl.AddAccessRule((New-Object System.Security.AccessControl.RegistryAccessRule((New-Object System.Security.Principal.SecurityIdentifier $sidUsers), 'SetValue', 'None', 'None', 'Allow')))
    $k.SetAccessControl($acl)
    $k.Close()
    $e = ErrorOf { Set-IemSshShell @common }
    Assert ($e -like "*may be changed by $sidUsers*" -and (Read-Values) -eq $absent -and $null -eq (Get-UndoTask) -and -not (Test-Path -LiteralPath $prior)) "set-refuses-a-key-users-may-write-and-writes-nothing ($e)"
    New-TestKey
    # A rule that reaches only subkeys (inherit-only) gives no right on the key's own values.
    $k = $hklm.OpenSubKey($sub, [Microsoft.Win32.RegistryKeyPermissionCheck]::ReadWriteSubTree, [System.Security.AccessControl.RegistryRights]::ChangePermissions -bor [System.Security.AccessControl.RegistryRights]::ReadPermissions)
    $acl = $k.GetAccessControl()
    $acl.AddAccessRule((New-Object System.Security.AccessControl.RegistryAccessRule((New-Object System.Security.Principal.SecurityIdentifier $sidUsers), 'FullControl', 'ContainerInherit', 'InheritOnly', 'Allow')))
    $k.SetAccessControl($acl)
    $k.Close()
    $k = $hklm.OpenSubKey($sub, $true)
    $k.SetValue('DefaultShell', [byte[]]@(), [Microsoft.Win32.RegistryValueKind]::None)
    $k.Close()
    # The key's rules are read first: this refusal names the value, so the inherit-only rule passed.
    $e = ErrorOf { Set-IemSshShell @common }
    Assert ($e -like '*DefaultShell*cannot be saved*' -and $null -eq (Get-UndoTask) -and -not (Test-Path -LiteralPath $prior)) "set-passes-an-inherit-only-rule-and-refuses-a-value-it-cannot-write-back-exactly ($e)"
    New-TestKey

    # ---- from absent: saved, armed, written, read back ----
    $before = Get-Date
    $r = Set-IemSshShell @common
    Assert ($r.state -ceq 'set' -and (Read-Values) -ceq $ours) "set-from-absent-writes-the-three-values ($(Read-Values))"
    foreach ($n in $names) { Assert ((Get-IemProp $r.prior $n) -eq $null) "set-from-absent-saves-$n-absent" }
    foreach ($p in @($dir, $prior, (Join-Path $dir 'IemSshShell.psm1'), (Join-Path $dir 'IemPc.psm1'), (Join-Path $dir 'ssh-shell-undo.ps1'))) {
        $bad = Test-IemElevatedItem -Path $p -UserSid $me.sid
        Assert ($bad.Count -eq 0) "set-writes-admin-only [$p] ($($bad -join '; '))"
    }
    Assert ((Get-FileHash -LiteralPath (Join-Path $dir 'IemPc.psm1')).Hash -ceq (Get-FileHash -LiteralPath (Join-Path $here 'IemPc.psm1')).Hash) 'set-copies-the-iempc-module-it-loaded'
    Assert ((Get-FileHash -LiteralPath (Join-Path $dir 'IemSshShell.psm1')).Hash -ceq (Get-FileHash -LiteralPath (Join-Path $here 'IemSshShell.psm1')).Hash) 'set-copies-itself'
    $t = Get-UndoTask
    Assert ($null -ne $t) 'set-arms-the-undo-task'
    $d = $t.Definition
    Assert ((SidOf ([string]$d.Principal.UserId)) -ceq 'S-1-5-18' -and
            [int]$d.Principal.LogonType -eq 5 -and [int]$d.Principal.RunLevel -eq 1) "undo-task-runs-as-system-highest ($($d.Principal.UserId), $($d.Principal.LogonType), $($d.Principal.RunLevel))"
    $tr = @($d.Triggers)
    $start = [datetime]$tr[0].StartBoundary
    Assert ($tr.Count -eq 1 -and [int]$tr[0].Type -eq 1 -and $start -ge $before.AddMinutes(9.5) -and $start -le (Get-Date).AddMinutes(10.5)) "undo-task-fires-once-in-ten-minutes ($($tr[0].StartBoundary))"
    Assert ([datetime]$r.undo.at -eq $start -and $r.undo.task -ceq ($folder + '\' + $taskName)) "set-answers-the-undo-task-and-its-time ($($r.undo.task) $($r.undo.at))"
    $a = @($d.Actions)[0]
    Assert ($a.Path -ceq (Join-Path ([Environment]::GetFolderPath('System')) 'WindowsPowerShell\v1.0\powershell.exe') -and
            $a.Arguments -clike ('*-File "' + (Join-Path $dir 'ssh-shell-undo.ps1') + '" -Key "' + $key + '" *')) "undo-task-runs-the-admin-only-entry-script ($($a.Path) $($a.Arguments))"
    Assert ($d.Settings.StartWhenAvailable -and -not $d.Settings.AllowHardTerminate) 'undo-task-starts-after-a-missed-start-and-is-never-ended-hard'
    Assert (Test-IemUndoTaskSddl -Sddl $t.GetSecurityDescriptor(4)) "undo-task-only-administrators-and-system ($($t.GetSecurityDescriptor(4)))"

    # ---- idempotent: a pending undo is re-armed with the saved values kept ----
    $savedBytes = [IO.File]::ReadAllBytes($prior)
    Start-Sleep -Milliseconds 1100
    $r = Set-IemSshShell @common
    Assert ($r.state -ceq 'rearmed' -and (Read-Values) -ceq $ours) "set-again-while-pending-rearms ($($r.state))"
    Assert ((Get-IemBytesSha256 -Bytes ([IO.File]::ReadAllBytes($prior))) -ceq (Get-IemBytesSha256 -Bytes $savedBytes)) 'set-again-keeps-the-first-saved-values'
    Assert ([datetime]$r.undo.at -gt $start) "set-again-moves-the-undo-time ($($r.undo.at))"

    # ---- confirm: the task and the saved values go, the values stay ----
    $c = Confirm-IemSshShell @common
    Assert ($c.state -ceq 'confirmed' -and $null -eq (Get-UndoTask) -and -not (Test-Path -LiteralPath $prior) -and (Read-Values) -ceq $ours) "confirm-removes-the-undo-task-and-the-saved-values ($($c.state))"
    $c = Confirm-IemSshShell @common
    Assert ($c.state -ceq 'unchanged') 'confirm-again-is-unchanged'
    $r = Set-IemSshShell @common
    Assert ($r.state -ceq 'unchanged' -and $null -eq (Get-UndoTask) -and -not (Test-Path -LiteralPath $prior)) "set-when-ours-and-nothing-pending-is-unchanged ($($r.state))"
    $u = Undo-IemSshShell @common
    Assert ($u.state -ceq 'nothing-saved' -and (Read-Values) -ceq $ours) "undo-with-nothing-saved-changes-nothing ($($u.state))"

    # ---- undo restores absent values (deleted) ----
    New-TestKey
    $r = Set-IemSshShell @common
    Assert ($r.state -ceq 'set') 'set-again-from-absent'
    $u = Undo-IemSshShell @common
    Assert ($u.state -ceq 'restored' -and (Read-Values) -ceq $absent -and $null -eq (Get-UndoTask) -and -not (Test-Path -LiteralPath $prior)) "undo-restores-absent-values ($(Read-Values))"

    # ---- a foreign prior shell: saved and restored exactly, by hand and by the task ----
    Set-Foreign
    Assert ((Read-Values) -ceq $foreign) "foreign-prior-in-place ($(Read-Values))"
    $r = Set-IemSshShell @common
    Assert ($r.state -ceq 'set' -and (Read-Values) -ceq $ours) 'set-from-a-foreign-shell'
    Assert ($r.prior.DefaultShellCommandOption.kind -ceq 'ExpandString' -and $r.prior.DefaultShellCommandOption.data -ceq '-c %IEMTESTVAR%') 'set-saves-an-expandable-string-unexpanded'
    $u = Undo-IemSshShell @common
    Assert ($u.state -ceq 'restored' -and (Read-Values) -ceq $foreign) "undo-restores-a-foreign-shell-exactly ($(Read-Values))"

    # ---- confirm refuses values that are not ours, and keeps the undo armed ----
    $r = Set-IemSshShell @common
    $k = $hklm.OpenSubKey($sub, $true)
    $k.SetValue('DefaultShellArguments', '/x', [Microsoft.Win32.RegistryValueKind]::String)
    $k.Close()
    $e = ErrorOf { Confirm-IemSshShell @common }
    Assert ($e -like '*not ours*' -and $null -ne (Get-UndoTask) -and (Test-Path -LiteralPath $prior)) "confirm-refuses-values-that-are-not-ours ($e)"

    # ---- a saved file someone else may change is never restored from ----
    $facl = Get-Acl -LiteralPath $prior
    $facl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule((New-Object System.Security.Principal.SecurityIdentifier $sidUsers), 'Modify', 'Allow')))
    Set-Acl -LiteralPath $prior -AclObject $facl
    $e = ErrorOf { Undo-IemSshShell @common }
    Assert ($e -like '*saved values are refused*' -and (Read-Values) -like '*String:/x') "undo-refuses-a-saved-file-others-may-change ($e)"
    $e = ErrorOf { Set-IemSshShell @common }
    Assert ($e -like '*saved values are refused*') "set-refuses-a-saved-file-others-may-change ($e)"
    $facl = Get-Acl -LiteralPath $prior
    [void]$facl.RemoveAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule((New-Object System.Security.Principal.SecurityIdentifier $sidUsers), 'Modify', 'Allow')))
    Set-Acl -LiteralPath $prior -AclObject $facl

    # ---- the undo task as registered, run now: SYSTEM restores the foreign shell ----
    $t = Get-UndoTask
    [void]$t.Run($null)
    $gone = Wait-UndoTaskGone 120
    $log = Join-Path $dir 'undo.log'
    $said = ''
    if (Test-Path -LiteralPath $log) { $said = ([IO.File]::ReadAllText($log)).Trim() }
    Assert ($gone -and (Read-Values) -ceq $foreign) "undo-task-restores-the-prior-shell-as-system ($(Read-Values); log: $said)"
    Assert ($said -like '*undo restored') "undo-task-logs-its-result ($said)"
    $r = Set-IemSshShell @common
    Assert ($r.state -ceq 'set' -and $r.undo_log -like '*undo restored') "set-names-the-last-undo ($($r.undo_log))"
    [void](Confirm-IemSshShell @common)
} finally {
    $sch = New-Object -ComObject 'Schedule.Service'
    $sch.Connect()
    try {
        $tf = $sch.GetFolder($folder)
        foreach ($t in @($tf.GetTasks(1))) { $tf.DeleteTask($t.Name, 0) }
        $sch.GetFolder('\').DeleteFolder($folder.TrimStart('\'), 0)
    } catch { Write-Host "cleanup: task folder $folder ($($_.Exception.Message))" }
    try { if ($null -ne $hklm.OpenSubKey($sub)) { $hklm.DeleteSubKeyTree($sub) } } catch { Write-Host "cleanup: $key ($($_.Exception.Message))" }
    Remove-Item -LiteralPath $base -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host 'Test-IemSshShell: all passed'
