#Requires -Version 5.1
# Self-test of IemSshShell.psm1 (#15, the admin-only OpenSSH default shell) on
# Windows PowerShell 5.1 (CI job windows, an ephemeral administrator runner).
# The module runs as the stage holds it: next to IemPc.psm1 and S1c's
# IemTuningStore.psm1 (its exact registry save and restore), copied into one
# folder. Every function runs against a TEST key
# (HKLM:\SOFTWARE\iemmixer-ssh-test-<id>, made admin-only as the real one must
# be), a test task folder \iemmixer-test-ssh-<id>\ and a temp elevated root:
# never HKLM:\SOFTWARE\OpenSSH and never a task of \iemmixer. Set gets the
# modules' sha256 as iempc passes them. The undo task is run once as
# registered (SYSTEM, its entry script and module copies); a stand-in under its
# name that waits for a marker file shows the refusals while it runs. The probe
# iempc composes runs through cmd.exe exactly as sshd starts it, with and
# without /d, and iempc_sshshell.parse_probe judges both. Only its own test
# objects are removed.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
function Assert($cond, $what) { if (-not $cond) { throw "FAILED: $what" } ; Write-Host "ok  $what" }
function ErrorOf([scriptblock]$b) { try { & $b; return '' } catch { return "$_" } }
function Sorted($items) { return ((@($items) | ForEach-Object { "$_" } | Sort-Object) -join ',') }

$id = [guid]::NewGuid().ToString('N').Substring(0, 8)
$base = Join-Path ([IO.Path]::GetTempPath()) ('iem-sshshell-' + $id)
$mods = Join-Path $base 'modules'
$sources = @{ 'IemPc.psm1' = (Join-Path $here 'IemPc.psm1'); 'IemSshShell.psm1' = (Join-Path $here 'IemSshShell.psm1')
              'IemTuningStore.psm1' = (Join-Path (Split-Path -Parent $here) 'pc-tuning\IemTuningStore.psm1') }
New-Item -ItemType Directory -Force -Path $mods | Out-Null
foreach ($n in @($sources.Keys)) { Copy-Item -LiteralPath $sources[$n] -Destination (Join-Path $mods $n) }
Import-Module (Join-Path $mods 'IemSshShell.psm1') -Force

$er = Join-Path $base 'elevated'
$sub = 'SOFTWARE\iemmixer-ssh-test-' + $id
$key = 'HKLM:\' + $sub
$folder = '\iemmixer-test-ssh-' + $id
$taskName = 'iemmixer-ssh-shell-undo'
$me = Resolve-IemUser
$cmd = Join-Path ([Environment]::GetFolderPath('System')) 'cmd.exe'
$common = @{ Key = $key; TaskFolder = $folder; ElevatedRoot = $er }
# What iempc passes: the sha256 of each module copy as the dev box checked it.
$sums = @{}
foreach ($n in @($sources.Keys)) { $sums[$n] = (Get-FileHash -LiteralPath (Join-Path $mods $n) -Algorithm SHA256).Hash.ToLowerInvariant() }
$setArgs = $common + @{ ModuleSha256 = $sums }
$dir = Join-Path $er 'ssh-shell'
$prior = Join-Path $dir 'prior.json'
$log = Join-Path $dir 'undo.log'
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

function Add-KeyRule([string]$Rights, [string]$Inherit, [string]$Propagate) {
    # One more allow rule for Users on the test key.
    $k = $hklm.OpenSubKey($sub, [Microsoft.Win32.RegistryKeyPermissionCheck]::ReadWriteSubTree,
        [System.Security.AccessControl.RegistryRights]::ChangePermissions -bor [System.Security.AccessControl.RegistryRights]::ReadPermissions)
    try {
        $acl = $k.GetAccessControl()
        $acl.AddAccessRule((New-Object System.Security.AccessControl.RegistryAccessRule((New-Object System.Security.Principal.SecurityIdentifier $sidUsers),
            [System.Security.AccessControl.RegistryRights]$Rights, $Inherit, $Propagate, 'Allow')))
        $k.SetAccessControl($acl)
    } finally { $k.Close() }
}

function Set-TestValue([string]$Name, $Data, [Microsoft.Win32.RegistryValueKind]$Kind) {
    $k = $hklm.OpenSubKey($sub, $true)
    try { $k.SetValue($Name, $Data, $Kind) } finally { $k.Close() }
}

function Set-Foreign {
    # A prior shell of another kind each: a string, an unexpanded expandable
    # string and a multi-string (Undo must write back exactly these).
    Set-TestValue 'DefaultShell' 'C:\Tools\othershell.exe' ([Microsoft.Win32.RegistryValueKind]::String)
    Set-TestValue 'DefaultShellCommandOption' '-c %IEMTESTVAR%' ([Microsoft.Win32.RegistryValueKind]::ExpandString)
    Set-TestValue 'DefaultShellArguments' ([string[]]@('-a', 'b c')) ([Microsoft.Win32.RegistryValueKind]::MultiString)
}

function Read-Values {
    # Each value as "<kind>:<data>" (unexpanded; a multi-string as its count
    # and its items joined by |), or "absent".
    $k = $hklm.OpenSubKey($sub)
    try {
        $out = @()
        foreach ($n in $names) {
            if (@($k.GetValueNames()) -notcontains $n) { $out += 'absent'; continue }
            $d = $k.GetValue($n, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            if ($d -is [array]) { $d = ('{0}:{1}' -f @($d).Count, (@($d) -join '|')) }
            $out += ('{0}:{1}' -f $k.GetValueKind($n), $d)
        }
        return ($out -join ' ; ')
    } finally { $k.Close() }
}

$ours = "String:$cmd ; String:/d /c ; String:/d"
$absent = 'absent ; absent ; absent'
$foreign = 'String:C:\Tools\othershell.exe ; ExpandString:-c %IEMTESTVAR% ; MultiString:2:-a|b c'

function SidOf([string]$Name) {
    # A task principal's UserId: Task Scheduler may give a name or the SID itself.
    if ($Name -cmatch '^S-1-[0-9-]+$') { return $Name }
    return (New-Object System.Security.Principal.NTAccount $Name).Translate([System.Security.Principal.SecurityIdentifier]).Value
}

function Get-UndoTask {
    $sch = Connect-IemScheduler
    return (Get-IemRegisteredTask -Scheduler $sch -Folder $folder -Name $taskName)
}

function Get-LogLines {
    if (-not (Test-Path -LiteralPath $log)) { return 0 }
    return @([IO.File]::ReadAllLines($log) | Where-Object { $_.Trim() }).Count
}

function Wait-UndoDone([int]$LogLines, [int]$Seconds) {
    # The undo task removes itself and the saved values, then its entry
    # appends one line to undo.log: all three, within a bound. Never ended by force.
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while ($clock.Elapsed.TotalSeconds -lt $Seconds) {
        if ($null -eq (Get-UndoTask) -and -not (Test-Path -LiteralPath $prior) -and (Get-LogLines) -gt $LogLines) { return $true }
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
    $outRead = $p.StandardOutput.ReadToEndAsync()
    $errRead = $p.StandardError.ReadToEndAsync()
    $p.StandardInput.Write($script)
    $p.StandardInput.Close()
    if (-not $p.WaitForExit(120000)) { throw 'the probe still runs after 120 s (left running, never force-ended)' }
    $last = @($outRead.Result -split "`r?`n" | Where-Object { $_.Trim() })[-1]
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
    $e = ErrorOf { Set-IemSshShell @setArgs }
    Assert ($e -like '*elevated root*refused*' -and (Read-Values) -eq $absent -and $null -eq (Get-UndoTask)) "set-refuses-a-missing-elevated-root ($e)"
    New-Item -ItemType Directory -Force -Path $er | Out-Null
    $e = ErrorOf { Set-IemSshShell @setArgs }
    Assert ($e -like '*elevated root*refused*' -and (Read-Values) -eq $absent -and $null -eq (Get-UndoTask) -and -not (Test-Path -LiteralPath $dir)) "set-refuses-an-elevated-root-the-user-may-change ($e)"
    Remove-Item -LiteralPath $er -Recurse -Force
    Install-IemElevatedFolder -Path $er -UserSid $me.sid
    Add-KeyRule 'SetValue' 'None' 'None'
    $e = ErrorOf { Set-IemSshShell @setArgs }
    Assert ($e -like "*may be changed by $sidUsers*" -and (Read-Values) -eq $absent -and $null -eq (Get-UndoTask) -and -not (Test-Path -LiteralPath $prior)) "set-refuses-a-key-users-may-write-and-writes-nothing ($e)"
    New-TestKey
    # A rule that reaches only subkeys (inherit-only) gives no right on the key's own values;
    # the key's rules are read first, so a refusal naming the value proves the rule passed.
    Add-KeyRule 'FullControl' 'ContainerInherit' 'InheritOnly'
    Set-TestValue 'DefaultShell' ([byte[]]@(1)) ([Microsoft.Win32.RegistryValueKind]::None)
    $e = ErrorOf { Set-IemSshShell @setArgs }
    Assert ($e -like '*DefaultShell*kind None refused*' -and $null -eq (Get-UndoTask) -and -not (Test-Path -LiteralPath $prior)) "set-passes-an-inherit-only-rule-and-refuses-a-value-it-cannot-write-back-exactly ($e)"
    New-TestKey
    # A module copy that is not the build iempc checked is never copied for the undo task.
    $wrong = $sums.Clone()
    $wrong['IemPc.psm1'] = '0' * 64
    $e = ErrorOf { Set-IemSshShell @common -ModuleSha256 $wrong }
    Assert ($e -like '*IemPc.psm1*not the build*' -and (Read-Values) -eq $absent -and $null -eq (Get-UndoTask) -and -not (Test-Path -LiteralPath $dir)) "set-refuses-a-module-that-is-not-the-checked-build-and-copies-nothing ($e)"

    # ---- a key that does not exist yet is made admin-only before any value is written ----
    $hklm.DeleteSubKeyTree($sub)
    $r = Set-IemSshShell @setArgs
    # Through the registry API: Windows PowerShell 5.1's Get-Acl -LiteralPath hands a registry
    # key on as its bare provider path, which it then cannot find (PowerShell #13107).
    $kk = $hklm.OpenSubKey($sub, [Microsoft.Win32.RegistryKeyPermissionCheck]::ReadSubTree, [System.Security.AccessControl.RegistryRights]::ReadPermissions)
    try { $kacl = $kk.GetAccessControl() } finally { $kk.Close() }
    $userWrite = @($kacl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]) | Where-Object {
        $_.IdentityReference.Value -ne 'S-1-5-32-544' -and $_.IdentityReference.Value -ne 'S-1-5-18' -and ([int]$_.RegistryRights -band 0x500D0026) -ne 0 })
    $explicit = Sorted @($kacl.GetAccessRules($true, $false, [System.Security.Principal.SecurityIdentifier]) | ForEach-Object { '{0}={1}' -f $_.IdentityReference.Value, [int]$_.RegistryRights })
    Assert ($r.state -ceq 'set' -and (Read-Values) -ceq $ours -and $kacl.GetOwner([System.Security.Principal.SecurityIdentifier]).Value -ceq 'S-1-5-32-544' -and
            $kacl.AreAccessRulesProtected -and $userWrite.Count -eq 0 -and
            $explicit -ceq (Sorted @('S-1-5-32-544=983103', 'S-1-5-18=983103', 'S-1-5-32-545=131097'))) "set-creates-a-missing-key-admin-only-nothing-inherited ($($kacl.GetSecurityDescriptorSddlForm('All')))"
    $u = Undo-IemSshShell @common
    Assert ($u.state -ceq 'restored' -and (Read-Values) -ceq $absent) "undo-after-a-created-key-deletes-the-values ($(Read-Values))"
    New-TestKey

    # ---- from absent: saved, armed, written, read back ----
    $before = Get-Date
    $r = Set-IemSshShell @setArgs
    Assert ($r.state -ceq 'set' -and (Read-Values) -ceq $ours) "set-from-absent-writes-the-three-values ($(Read-Values))"
    foreach ($n in $names) { Assert ([string](Get-IemProp (Get-IemProp $r.prior $n) 'kind') -ceq 'absent') "set-from-absent-saves-$n-absent" }
    foreach ($p in @($dir, $prior, (Join-Path $dir 'ssh-shell-undo.ps1'))) {
        $bad = Test-IemElevatedItem -Path $p -UserSid $me.sid
        Assert ($bad.Count -eq 0) "set-writes-admin-only [$p] ($($bad -join '; '))"
    }
    foreach ($n in @($sources.Keys)) {
        $copy = Join-Path $dir $n
        $bad = Test-IemElevatedItem -Path $copy -UserSid $me.sid
        Assert ($bad.Count -eq 0 -and (Get-FileHash -LiteralPath $copy).Hash -ceq (Get-FileHash -LiteralPath $sources[$n]).Hash) "set-copies-the-module-the-undo-loads-admin-only [$n] ($($bad -join '; '))"
    }
    $t = Get-UndoTask
    Assert ($null -ne $t) 'set-arms-the-undo-task'
    $d = $t.Definition
    Assert ((SidOf ([string]$d.Principal.UserId)) -ceq 'S-1-5-18' -and
            [int]$d.Principal.LogonType -eq 5 -and [int]$d.Principal.RunLevel -eq 1) "undo-task-runs-as-system-highest ($($d.Principal.UserId), $($d.Principal.LogonType), $($d.Principal.RunLevel))"
    $tr = @($d.Triggers)
    $start = [datetime]$tr[0].StartBoundary
    Assert ($tr.Count -eq 1 -and [int]$tr[0].Type -eq 1 -and $start -ge $before.AddMinutes(9.5) -and $start -le (Get-Date).AddMinutes(10.5)) "undo-task-fires-once-in-ten-minutes ($($tr[0].StartBoundary))"
    $next = [datetime]$t.NextRunTime
    Assert ($next -ge $before.AddMinutes(9.5) -and $next -le (Get-Date).AddMinutes(10.5)) "undo-task-next-run-is-in-ten-minutes-local-time ($next)"
    Assert ([datetime]$r.undo.at -eq $start -and $r.undo.task -ceq ($folder + '\' + $taskName)) "set-answers-the-undo-task-and-its-time ($($r.undo.task) $($r.undo.at))"
    $a = @($d.Actions)[0]
    Assert ($a.Path -ceq (Join-Path ([Environment]::GetFolderPath('System')) 'WindowsPowerShell\v1.0\powershell.exe') -and
            $a.Arguments -clike ('*-File "' + (Join-Path $dir 'ssh-shell-undo.ps1') + '" -Key "' + $key + '" *')) "undo-task-runs-the-admin-only-entry-script ($($a.Path) $($a.Arguments))"
    Assert ($d.Settings.StartWhenAvailable -and -not $d.Settings.AllowHardTerminate) 'undo-task-starts-after-a-missed-start-and-is-never-ended-hard'
    Assert (Test-IemUndoTaskSddl -Sddl $t.GetSecurityDescriptor(4)) "undo-task-only-administrators-and-system ($($t.GetSecurityDescriptor(4)))"

    # ---- idempotent: a pending undo is re-armed with the saved values kept ----
    $savedBytes = [IO.File]::ReadAllBytes($prior)
    Start-Sleep -Milliseconds 1100
    $r = Set-IemSshShell @setArgs
    Assert ($r.state -ceq 'rearmed' -and (Read-Values) -ceq $ours) "set-again-while-pending-rearms ($($r.state))"
    Assert ((Get-IemBytesSha256 -Bytes ([IO.File]::ReadAllBytes($prior))) -ceq (Get-IemBytesSha256 -Bytes $savedBytes)) 'set-again-keeps-the-first-saved-values'
    Assert ([datetime]$r.undo.at -gt $start) "set-again-moves-the-undo-time ($($r.undo.at))"

    # ---- an undo task that is queued or runs: Set and Confirm refuse and change nothing ----
    $marker = Join-Path $base 'release'
    $psExe = Join-Path ([Environment]::GetFolderPath('System')) 'WindowsPowerShell\v1.0\powershell.exe'
    # It ends by itself once the marker exists, at the latest after 90 s (never ended by force).
    $waitArgs = '-NoProfile -NonInteractive -Command "$t = [DateTime]::UtcNow.AddSeconds(90); while (-not (Test-Path -LiteralPath ''' +
        $marker + ''') -and [DateTime]::UtcNow -lt $t) { Start-Sleep -Milliseconds 200 }"'
    Register-ScheduledTask -TaskPath ($folder + '\') -TaskName $taskName -Action (New-ScheduledTaskAction -Execute $psExe -Argument $waitArgs) `
        -Principal (New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest) -Force | Out-Null
    Start-ScheduledTask -TaskPath ($folder + '\') -TaskName $taskName
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while ("$((Get-ScheduledTask -TaskPath ($folder + '\') -TaskName $taskName).State)" -ne 'Running' -and $clock.Elapsed.TotalSeconds -lt 30) { Start-Sleep -Milliseconds 200 }
    $savedBytes = [IO.File]::ReadAllBytes($prior)
    $e = ErrorOf { Set-IemSshShell @setArgs }
    Assert ($e -like '*queued or runs*' -and (Read-Values) -ceq $ours -and
            (Get-IemBytesSha256 -Bytes ([IO.File]::ReadAllBytes($prior))) -ceq (Get-IemBytesSha256 -Bytes $savedBytes)) "set-refuses-while-the-undo-task-runs ($e)"
    $e = ErrorOf { Confirm-IemSshShell @common }
    Assert ($e -like '*queued or runs*' -and (Test-Path -LiteralPath $prior) -and $null -ne (Get-UndoTask)) "confirm-refuses-while-the-undo-task-runs ($e)"
    New-Item -ItemType File -Path $marker | Out-Null
    $clock.Restart()
    while ("$((Get-ScheduledTask -TaskPath ($folder + '\') -TaskName $taskName).State)" -eq 'Running' -and $clock.Elapsed.TotalSeconds -lt 60) { Start-Sleep -Milliseconds 200 }
    Assert ("$((Get-ScheduledTask -TaskPath ($folder + '\') -TaskName $taskName).State)" -ne 'Running') 'the-busy-stand-in-ended-by-itself'

    # ---- a re-arm puts the undo task back and rewrites a copy that changed ----
    $pcCopy = Join-Path $dir 'IemPc.psm1'
    [IO.File]::AppendAllText($pcCopy, "`r`n# changed after the set`r`n")
    $r = Set-IemSshShell @setArgs
    $bad = Test-IemElevatedItem -Path $pcCopy -UserSid $me.sid
    Assert ($r.state -ceq 'rearmed' -and $bad.Count -eq 0 -and (Get-FileHash -LiteralPath $pcCopy).Hash.ToLowerInvariant() -ceq $sums['IemPc.psm1']) "set-rewrites-an-undo-copy-that-changed ($($bad -join '; '))"
    Assert (@((Get-UndoTask).Definition.Actions)[0].Arguments -clike '*ssh-shell-undo.ps1*') 'set-again-registers-the-undo-action-again'

    # ---- confirm refuses a key others may change, and keeps the undo armed ----
    Add-KeyRule 'SetValue' 'None' 'None'
    $e = ErrorOf { Confirm-IemSshShell @common }
    Assert ($e -like "*may be changed by $sidUsers*" -and $null -ne (Get-UndoTask) -and (Test-Path -LiteralPath $prior)) "confirm-refuses-a-key-others-may-change ($e)"
    $k = $hklm.OpenSubKey($sub, [Microsoft.Win32.RegistryKeyPermissionCheck]::ReadWriteSubTree,
        [System.Security.AccessControl.RegistryRights]::ChangePermissions -bor [System.Security.AccessControl.RegistryRights]::ReadPermissions)
    $acl = $k.GetAccessControl()
    [void]$acl.RemoveAccessRule((New-Object System.Security.AccessControl.RegistryAccessRule((New-Object System.Security.Principal.SecurityIdentifier $sidUsers),
        [System.Security.AccessControl.RegistryRights]::SetValue, 'None', 'None', 'Allow')))
    $k.SetAccessControl($acl)
    $k.Close()

    # ---- confirm: the saved values and the task go, the values stay ----
    $c = Confirm-IemSshShell @common
    Assert ($c.state -ceq 'confirmed' -and $null -eq (Get-UndoTask) -and -not (Test-Path -LiteralPath $prior) -and (Read-Values) -ceq $ours) "confirm-removes-the-undo-task-and-the-saved-values ($($c.state))"
    $c = Confirm-IemSshShell @common
    Assert ($c.state -ceq 'unchanged') 'confirm-again-is-unchanged'
    $r = Set-IemSshShell @setArgs
    Assert ($r.state -ceq 'unchanged' -and $null -eq (Get-UndoTask) -and -not (Test-Path -LiteralPath $prior)) "set-when-ours-and-nothing-pending-is-unchanged ($($r.state))"
    $u = Undo-IemSshShell @common
    Assert ($u.state -ceq 'nothing-saved' -and (Read-Values) -ceq $ours) "undo-with-nothing-saved-changes-nothing ($($u.state))"

    # ---- undo restores absent values (deleted) ----
    New-TestKey
    $r = Set-IemSshShell @setArgs
    Assert ($r.state -ceq 'set') 'set-again-from-absent'
    $u = Undo-IemSshShell @common
    Assert ($u.state -ceq 'restored' -and (Read-Values) -ceq $absent -and $null -eq (Get-UndoTask) -and -not (Test-Path -LiteralPath $prior)) "undo-restores-absent-values ($(Read-Values))"

    # ---- an empty multi-string is no absent value and no empty string ----
    Set-TestValue 'DefaultShellArguments' ([string[]]@()) ([Microsoft.Win32.RegistryValueKind]::MultiString)
    $empty = 'absent ; absent ; MultiString:0:'
    Assert ((Read-Values) -ceq $empty) "empty-multi-string-in-place ($(Read-Values))"
    $r = Set-IemSshShell @setArgs
    $u = Undo-IemSshShell @common
    Assert ($u.state -ceq 'restored' -and (Read-Values) -ceq $empty) "undo-restores-an-empty-multi-string-exactly ($(Read-Values))"
    New-TestKey

    # ---- a foreign prior shell: saved and restored exactly, by hand and by the task ----
    Set-Foreign
    Assert ((Read-Values) -ceq $foreign) "foreign-prior-in-place ($(Read-Values))"
    $r = Set-IemSshShell @setArgs
    Assert ($r.state -ceq 'set' -and (Read-Values) -ceq $ours) 'set-from-a-foreign-shell'
    Assert ($r.prior.DefaultShellCommandOption.kind -ceq 'ExpandString' -and $r.prior.DefaultShellCommandOption.data -ceq '-c %IEMTESTVAR%') 'set-saves-an-expandable-string-unexpanded'
    $u = Undo-IemSshShell @common
    Assert ($u.state -ceq 'restored' -and (Read-Values) -ceq $foreign) "undo-restores-a-foreign-shell-exactly ($(Read-Values))"

    # ---- confirm refuses values that are not ours, and keeps the undo armed ----
    $r = Set-IemSshShell @setArgs
    Set-TestValue 'DefaultShellArguments' '/x' ([Microsoft.Win32.RegistryValueKind]::String)
    $e = ErrorOf { Confirm-IemSshShell @common }
    Assert ($e -like '*not ours*' -and $null -ne (Get-UndoTask) -and (Test-Path -LiteralPath $prior)) "confirm-refuses-values-that-are-not-ours ($e)"

    # ---- a saved file someone else may change is never restored from ----
    $rule = New-Object System.Security.AccessControl.FileSystemAccessRule((New-Object System.Security.Principal.SecurityIdentifier $sidUsers), 'Modify', 'Allow')
    $facl = Get-Acl -LiteralPath $prior
    $facl.AddAccessRule($rule)
    Set-Acl -LiteralPath $prior -AclObject $facl
    # Undo never restores from it: it falls back to sshd's own default (the three values
    # deleted), keeps the file for inspection and removes the task.
    $u = Undo-IemSshShell @common
    Assert ($u.state -ceq 'default' -and "$($u.why)" -like '*saved values are refused*' -and (Read-Values) -ceq $absent -and
            (Test-Path -LiteralPath $prior) -and $null -eq (Get-UndoTask)) "undo-from-saved-values-others-may-change-falls-back-to-sshd-s-default ($($u.state); $(Read-Values))"
    $e = ErrorOf { Set-IemSshShell @setArgs }
    Assert ($e -like '*saved values are refused*') "set-refuses-a-saved-file-others-may-change ($e)"
    $facl = Get-Acl -LiteralPath $prior
    [void]$facl.RemoveAccessRule($rule)
    Set-Acl -LiteralPath $prior -AclObject $facl
    $r = Set-IemSshShell @setArgs
    Assert ($r.state -ceq 'rearmed' -and (Read-Values) -ceq $ours) "set-rearms-over-the-kept-saved-values ($($r.state))"

    # ---- the undo task as registered, run now: SYSTEM restores the foreign shell ----
    $lines = Get-LogLines
    [void](Get-UndoTask).Run($null)
    $done = Wait-UndoDone $lines 120
    $said = ''
    if (Test-Path -LiteralPath $log) { $said = ([IO.File]::ReadAllText($log)).Trim() }
    Assert ($done -and (Read-Values) -ceq $foreign) "undo-task-restores-the-prior-shell-as-system ($(Read-Values); log: $said)"
    Assert ($said -like '*undo restored') "undo-task-logs-its-result ($said)"
    $r = Set-IemSshShell @setArgs
    Assert ($r.state -ceq 'set' -and $r.undo_log -like '*undo restored') "set-names-the-last-undo ($($r.undo_log))"

    # ---- saved data not in its kind's form (binary hex with a line break): never written back;
    # the task falls back to sshd's default and logs why ----
    $j = [IO.File]::ReadAllText($prior) | ConvertFrom-Json
    $j.values.DefaultShell = [pscustomobject]@{ kind = 'Binary'; data = "ab`n" }
    [IO.File]::WriteAllText($prior, (ConvertTo-Json -InputObject $j -Depth 6), (New-Object System.Text.UTF8Encoding $false))
    $lines = Get-LogLines
    [void](Get-UndoTask).Run($null)
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while (((Get-LogLines) -le $lines -or $null -ne (Get-UndoTask)) -and $clock.Elapsed.TotalSeconds -lt 120) { Start-Sleep -Milliseconds 500 }
    $last = @([IO.File]::ReadAllLines($log) | Where-Object { $_.Trim() })[-1]
    Assert ($last -like '*undo default: *DefaultShell is no value Set-IemRegRaw writes back*' -and (Read-Values) -ceq $absent -and
            (Test-Path -LiteralPath $prior) -and $null -eq (Get-UndoTask)) "undo-task-falls-back-to-sshd-s-default-and-logs-why ($last; $(Read-Values))"
    $e = ErrorOf { Set-IemSshShell @setArgs }
    Assert ($e -like '*no value Set-IemRegRaw writes back*' -and (Read-Values) -ceq $absent) "set-refuses-saved-values-it-cannot-write-back ($e)"
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
