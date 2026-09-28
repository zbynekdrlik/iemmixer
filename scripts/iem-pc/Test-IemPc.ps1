#Requires -Version 5.1
# Self-test of the S6 PC module (IemPc.psm1) and hil-v1.ps1 on Windows
# PowerShell 5.1 (CI job windows, an ephemeral administrator runner), against
# real backends: a test task folder \iemmixer-test\ (security descriptors read
# back), a temp root, an HKCU test key, a disabled test firewall rule and a
# test service. Defender and the runner run with -WhatIf (no Defender to
# change there, no ops token). Only its own test objects are removed.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
# -Include is ignored next to -LiteralPath in Windows PowerShell 5.1: filter by extension.
foreach ($f in (Get-ChildItem -LiteralPath $here -File | Where-Object { @('.ps1', '.psm1') -contains $_.Extension })) {
    $tokens = $null; $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($f.FullName, [ref]$tokens, [ref]$errors)
    if ($errors.Count -gt 0) { throw "parse errors in $($f.Name): $($errors[0].Message) (line $($errors[0].Extent.StartLineNumber))" }
    Write-Host "ok  parses: $($f.Name)"
}
Import-Module (Join-Path $here 'IemPc.psm1') -Force
function Assert($cond, $what) { if (-not $cond) { throw "FAILED: $what" } ; Write-Host "ok  $what" }
function Throws([scriptblock]$b, $what) { $t = $false; try { & $b } catch { $t = $true }; Assert $t $what }
function ErrorOf([scriptblock]$b) { try { & $b; return '' } catch { return "$_" } }
function Sorted($items) { return ((@($items) | ForEach-Object { "$_" } | Sort-Object) -join ',') }

$id = [guid]::NewGuid().ToString('N').Substring(0, 8)
$base = Join-Path ([IO.Path]::GetTempPath()) ('iempc-test-' + $id)
$root = Join-Path $base 'root'
$elevated = Join-Path $base 'elevated'
$folder = '\iemmixer-test'
$ruleName = 'iemmixer-test-http-' + $id
$svcName = 'iemmixer-test-' + $id
$regKey = 'HKCU:\Software\iemmixer-iempc-test-' + $id
$appExe = Join-Path $env:SystemRoot 'System32\cmd.exe'
$me = Resolve-IemUser
New-Item -ItemType Directory -Force -Path $root | Out-Null
New-Item -Path $regKey -Force | Out-Null

try {
    # ---- task security descriptors (pure) ----
    $userSid = 'S-1-5-21-1000000001-1000000002-1000000003-1001'
    Assert ((Get-IemTaskSddl -UserSid $userSid) -ceq "D:(A;;GRGX;;;$userSid)(A;;FA;;;BA)(A;;FA;;;SY)") 'task-sddl-is-the-design-string'
    Throws { Get-IemTaskSddl -UserSid 'S-1-5-18' } 'task-sddl-refuses-a-non-user-sid'
    Assert (Test-IemTaskSddl -Sddl "D:(A;;GRGX;;;$userSid)(A;;FA;;;BA)(A;;FA;;;SY)" -UserSid $userSid) 'task-sddl-read-back-in-generic-form'
    Assert (Test-IemTaskSddl -Sddl "D:(A;;FA;;;SY)(A;;FRFX;;;$userSid)(A;;GA;;;BA)" -UserSid $userSid) 'task-sddl-read-back-in-file-form-any-order'
    $badSddl = @(
        "D:(A;;GRGX;;;$userSid)(A;;FA;;;BA)",
        "D:(A;;GRGX;;;$userSid)(A;;FA;;;BA)(A;;FA;;;SY)(A;;FRFX;;;AU)",
        "D:(A;;FA;;;$userSid)(A;;FA;;;BA)(A;;FA;;;SY)",
        "D:(A;;GRGXGW;;;$userSid)(A;;FA;;;BA)(A;;FA;;;SY)",
        "D:(A;;GR;;;$userSid)(A;;FA;;;BA)(A;;FA;;;SY)",
        "D:(A;;GRGX;;;$userSid)(A;;GRGX;;;BA)(A;;FA;;;SY)",
        "D:(D;;FA;;;WD)(A;;GRGX;;;$userSid)(A;;FA;;;BA)(A;;FA;;;SY)",
        "D:(A;;GRGX;;;$userSid)(A;;GRGX;;;$userSid)(A;;FA;;;BA)(A;;FA;;;SY)",
        'not an sddl', '')
    foreach ($bad in $badSddl) { Assert (-not (Test-IemTaskSddl -Sddl $bad -UserSid $userSid)) "task-sddl-read-back-refuses [$bad]" }
    # As the real Task Scheduler reads a task back (the windows runner, CI run
    # 36360472125): our three explicit ACEs (the user's GRGX as 0x1200a9, FRFX)
    # and ACEs inherited (ID) from the task folder for Administrators and SYSTEM.
    # The user is the synthetic SID: 'LA' would resolve to this machine's
    # administrator.
    $inherited = '(A;ID;0x1f019f;;;BA)(A;ID;0x1f019f;;;SY)(A;ID;FA;;;BA)'
    $real = "D:AI(A;;0x1200a9;;;$userSid)(A;;FA;;;BA)(A;;FA;;;SY)" + $inherited
    Assert (Test-IemTaskSddl -Sddl $real -UserSid $userSid) 'task-sddl-read-back-with-administrators-and-system-inherited-from-the-folder'
    Assert (Test-IemTaskSddl -Sddl ("D:AI(A;;FA;;;SY)(A;ID;FR;;;SY)(A;;GRGX;;;$userSid)(A;ID;GA;;;BA)(A;;GA;;;BA)") -UserSid $userSid) 'task-sddl-read-back-inherited-any-order-any-rights'
    $badInherited = @(
        ($real + '(A;ID;FRFX;;;BU)'),
        ($real + '(A;ID;FR;;;WD)'),
        ($real + '(A;ID;FRFX;;;AU)'),
        ($real + "(A;ID;FRFX;;;$userSid)"),
        ($real + "(A;ID;FA;;;$userSid)"),
        ($real + '(D;ID;FA;;;BU)'),
        ($real + '(D;ID;FA;;;BA)'),
        ($real + '(OA;ID;FA;00000000-0000-0000-0000-000000000001;;BA)'),
        ("D:AI(A;;0x1200a9;;;$userSid)(A;;FA;;;SY)" + $inherited),
        ("D:AI(A;;0x1200a9;;;$userSid)(A;;FA;;;BA)" + $inherited),
        ('D:AI(A;;FA;;;BA)(A;;FA;;;SY)' + $inherited),
        ("D:AI(A;;0x1200a9;;;$userSid)(A;;FA;;;BA)(A;;FA;;;BA)(A;;FA;;;SY)" + $inherited),
        ("D:AI(A;;0x1200a9;;;$userSid)(A;;FA;;;BA)(A;;FA;;;SY)(A;;FA;;;SY)" + $inherited),
        ("D:AI(A;;0x1200a9;;;$userSid)(A;;0x1200a9;;;$userSid)(A;;FA;;;BA)(A;;FA;;;SY)" + $inherited),
        ("D:AI(A;;FA;;;$userSid)(A;;FA;;;BA)(A;;FA;;;SY)" + $inherited),
        ("D:(A;ID;0x1f019f;;;BA)(A;ID;0x1f019f;;;SY)(A;ID;FA;;;BA)(A;;FR;;;$userSid)"))
    foreach ($bad in $badInherited) { Assert (-not (Test-IemTaskSddl -Sddl $bad -UserSid $userSid)) "task-sddl-read-back-refuses [$bad]" }

    # ---- Register-IemTasks on the real Task Scheduler ----
    $prefArgs = @{ PrefKey = $regKey; PrefName = 'Pref'; PrefOriginal = '64' }
    $sch = New-Object -ComObject 'Schedule.Service'
    $sch.Connect()
    $e = ErrorOf { Register-IemTasks -Root $root -AppExe $appExe -Folder '\iemmixer-test-none' -ElevatedRoot $elevated @prefArgs }
    Assert ($e -like '*iemmixer-StartREAPER is missing*') "tasks-need-the-existing-start-reaper-task ($e)"
    Throws { $sch.GetFolder('\iemmixer-test-none') } 'tasks-refused-before-any-folder-exists'
    Assert (-not (Test-Path -LiteralPath $elevated)) 'tasks-refused-before-the-elevated-folder'
    Throws { Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedRoot $elevated -PrefKey $regKey -PrefName 'Pref' -PrefOriginal '64x' } 'tasks-refuse-a-non-numeric-original'
    Throws { Register-IemTasks -Root ($root + '"') -AppExe $appExe -Folder $folder -ElevatedRoot $elevated @prefArgs } 'tasks-refuse-a-quote-in-a-path'

    Register-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'iemmixer-StartREAPER' `
        -Action (New-ScheduledTaskAction -Execute 'cmd.exe' -Argument '/c exit 0') `
        -Principal (New-ScheduledTaskPrincipal -UserId $me.name -LogonType Interactive -RunLevel Limited) | Out-Null
    $reports = Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedRoot $elevated @prefArgs
    $byName = @{}
    foreach ($r in $reports) { $byName[$r.task] = $r }
    Assert ((Sorted $byName.Keys) -ceq (Sorted @('iemmixer-guard', 'iemmixer-StartApp', 'iemmixer-probe', 'iemmixer-tuning', 'iemmixer-exclude', 'iemmixer-logon', 'iemmixer-StartREAPER'))) 'tasks-all-seven'
    foreach ($r in $reports) { Assert ($r.sddl_ok -and $r.problems.Count -eq 0) "tasks-$($r.task)-reads-back-with-our-descriptor" }

    # An independent read through the ScheduledTasks cmdlets.
    foreach ($n in @('iemmixer-guard', 'iemmixer-StartApp', 'iemmixer-probe', 'iemmixer-tuning', 'iemmixer-exclude', 'iemmixer-logon')) {
        $t = Get-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName $n
        $level = 'Limited'
        if (@('iemmixer-tuning', 'iemmixer-exclude', 'iemmixer-logon') -contains $n) { $level = 'Highest' }
        $st = $t.Settings
        Assert ("$($t.Principal.LogonType)" -eq 'Interactive' -and "$($t.Principal.RunLevel)" -eq $level) "tasks-$n-interactive-$level"
        Assert ($st.ExecutionTimeLimit -eq 'PT0S' -and "$($st.MultipleInstances)" -eq 'IgnoreNew' -and $st.RestartCount -eq 3 -and $st.RestartInterval -eq 'PT1M') "tasks-$n-unbounded-ignorenew-restart-3x1min"
        Assert (-not $st.DisallowStartIfOnBatteries -and -not $st.StopIfGoingOnBatteries -and -not $st.IdleSettings.StopOnIdleEnd -and -not $st.AllowHardTerminate -and $st.Priority -eq 4) "tasks-$n-no-battery-or-idle-stop-never-ended-hard-normal-priority"
    }
    $g = Get-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'iemmixer-guard'
    Assert ($g.Actions[0].Execute -eq (Join-Path $root 'bin\iemmixer-guard.exe') -and $g.Actions[0].Arguments -eq 'run' -and $g.Actions[0].WorkingDirectory -eq $root) 'tasks-guard-runs-the-bin-copy'
    Assert ($null -eq $g.Triggers -or @($g.Triggers).Count -eq 0) 'tasks-guard-has-no-trigger-before-cutover'
    $a = Get-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'iemmixer-StartApp'
    Assert ($a.Actions[0].Execute -eq $appExe -and -not $a.Actions[0].Arguments) 'tasks-start-app-runs-the-exe-directly'
    $p = Get-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'iemmixer-probe'
    Assert ($p.Actions[0].Arguments -eq '/c exit 0') 'tasks-probe-exits-0'
    $l = Get-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'iemmixer-logon'
    Assert (@($l.Triggers).Count -eq 1 -and $l.Triggers[0].CimClass.CimClassName -eq 'MSFT_TaskLogonTrigger' -and $l.Triggers[0].UserId -like "*$(Split-Path -Leaf $me.name)") 'tasks-logon-fires-at-the-users-logon'
    $etasks = Join-Path $elevated 'tasks'
    $eout = Join-Path $etasks 'out'
    $etuning = Join-Path $elevated 'tuning'
    $entry = Join-Path $etasks 'iem-task.ps1'
    Assert ($l.Actions[0].Arguments -like "*-File `"$entry`" -Root `"$root`" -TuningDir `"$etuning`" -Kind logon -PrefKey `"$regKey`" -PrefName `"Pref`" -PrefOriginal `"64`"") "tasks-logon-runs-the-elevated-entry ($($l.Actions[0].Arguments))"
    $tu = Get-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'iemmixer-tuning'
    Assert ($tu.Actions[0].Arguments -like "*-File `"$entry`" -Root `"$root`" -TuningDir `"$etuning`" -Kind tuning" -and $tu.Actions[0].Execute -like '*\WindowsPowerShell\v1.0\powershell.exe' -and $tu.Actions[0].WorkingDirectory -eq $etasks) 'tasks-tuning-runs-the-elevated-entry-with-its-tuning-folder'
    $sr = Get-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'iemmixer-StartREAPER'
    Assert ($sr.Actions[0].Execute -eq 'cmd.exe' -and $sr.Actions[0].Arguments -eq '/c exit 0') 'tasks-start-reaper-keeps-its-action'
    foreach ($n in @('iemmixer-StartREAPER', 'iemmixer-guard', 'iemmixer-exclude')) {
        $sd = $sch.GetFolder($folder).GetTask($n).GetSecurityDescriptor(4)
        Assert (Test-IemTaskSddl -Sddl $sd -UserSid $me.sid) "tasks-$n-descriptor-lets-the-user-run-it ($sd)"
    }
    # The elevated root: this module and the entry, the results and the tuning
    # folder, owned by Administrators and changeable only by them and SYSTEM.
    Assert ((Get-FileHash -LiteralPath (Join-Path $etasks 'IemPc.psm1')).Hash -eq (Get-FileHash -LiteralPath (Join-Path $here 'IemPc.psm1')).Hash) 'elevated-folder-holds-this-module'
    $tokens = $null; $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($entry, [ref]$tokens, [ref]$errors)
    Assert ($errors.Count -eq 0) 'elevated-entry-parses'
    foreach ($d in @($elevated, $etasks, $eout, $etuning)) {
        $ea = Get-Acl -LiteralPath $d
        $er = @($ea.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
        $userRule = @($er | Where-Object { $_.IdentityReference.Value -eq $me.sid })
        Assert ($ea.AreAccessRulesProtected -and $er.Count -eq 3 -and $userRule.Count -eq 1 -and [int]$userRule[0].FileSystemRights -eq 1179817) "elevated-folder-protected-user-reads-only [$d]"
        Assert ((Sorted ($er | ForEach-Object { $_.IdentityReference.Value })) -eq (Sorted @($me.sid, 'S-1-5-18', 'S-1-5-32-544'))) "elevated-folder-user-system-administrators [$d]"
        Assert ($ea.GetOwner([System.Security.Principal.SecurityIdentifier]).Value -eq 'S-1-5-32-544') "elevated-folder-owned-by-administrators [$d]"
    }
    foreach ($f in @((Join-Path $etasks 'IemPc.psm1'), $entry)) {
        $fa = Get-Acl -LiteralPath $f
        $fr = @($fa.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
        Assert ($fa.GetOwner([System.Security.Principal.SecurityIdentifier]).Value -eq 'S-1-5-32-544' -and $fr.Count -eq 3 -and @($fr | Where-Object { -not $_.IsInherited }).Count -eq 0) "elevated-file-owned-by-administrators-rules-inherited [$f]"
        $fb = Test-IemElevatedItem -Path $f -UserSid $me.sid
        Assert ($fb.Count -eq 0) "elevated-file-reads-back [$f] ($($fb -join '; '))"
    }
    $eb = Test-IemElevatedItem -Path $elevated -UserSid $me.sid
    Assert ($eb.Count -eq 0) "elevated-root-reads-back ($($eb -join '; '))"
    Throws { Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedRoot (Join-Path $root 'elevated') @prefArgs } 'tasks-refuse-an-elevated-root-inside-the-users-root'
    Throws { Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedRoot 'relative\elevated' @prefArgs } 'tasks-refuse-a-relative-elevated-root'
    # A descriptor loosened after registration (here Users may run the guard's
    # task) is put back by the next run: an update keeps a task's old descriptor,
    # so Register-IemTasks sets ours on every task, as it does on StartREAPER.
    $sch.GetFolder($folder).GetTask('iemmixer-guard').SetSecurityDescriptor("D:(A;;GRGX;;;$($me.sid))(A;;FA;;;BA)(A;;FA;;;SY)(A;;GRGX;;;BU)", 16)
    $sd = $sch.GetFolder($folder).GetTask('iemmixer-guard').GetSecurityDescriptor(4)
    Assert (-not (Test-IemTaskSddl -Sddl $sd -UserSid $me.sid)) "tasks-precondition-the-guard-task-loosened ($sd)"
    $again = Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedRoot $elevated @prefArgs
    Assert ($again.Count -eq 7) 'tasks-register-again-idempotent'
    $sd = $sch.GetFolder($folder).GetTask('iemmixer-guard').GetSecurityDescriptor(4)
    Assert (Test-IemTaskSddl -Sddl $sd -UserSid $me.sid) "tasks-register-again-restores-a-loosened-descriptor ($sd)"

    # ---- the root's DACL ----
    New-Item -ItemType Directory -Force -Path (Join-Path $root 'bundles') | Out-Null
    Set-Content -LiteralPath (Join-Path $root 'bundles\x.txt') -Value 'x'
    $acl = Get-Acl -LiteralPath $root
    $acl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule('Everyone', 'Read', 'ContainerInherit, ObjectInherit', 'None', 'Allow')))
    Set-Acl -LiteralPath $root -AclObject $acl
    $childRules = @((Get-Acl -LiteralPath (Join-Path $root 'bundles\x.txt')).GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
    Assert (@($childRules | Where-Object { $_.IdentityReference.Value -eq 'S-1-1-0' }).Count -eq 1) 'root-acl-precondition-everyone-reaches-a-child'
    # Items that hold rules of their own: a folder that inherits nothing (its own
    # protected rules, Everyone among them) and, created inside it, a file with
    # its own rule for Everyone.
    $everyone = New-Object System.Security.Principal.SecurityIdentifier 'S-1-1-0'
    $own = Join-Path $root 'bundles\own'
    $ownFile = Join-Path $own 'y.txt'
    New-Item -ItemType Directory -Force -Path $own | Out-Null
    $ownRights = Get-IemRootRights -UserSid $me.sid
    $ownRights['S-1-1-0'] = [System.Security.AccessControl.FileSystemRights]::ReadAndExecute
    Set-IemDirectoryAcl -Path $own -Rights $ownRights
    Set-Content -LiteralPath $ownFile -Value 'y'
    $ownFileAcl = [IO.File]::GetAccessControl($ownFile)
    $ownFileAcl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($everyone, 'Read', 'Allow')))
    [IO.File]::SetAccessControl($ownFile, $ownFileAcl)
    $ownAcl = Get-Acl -LiteralPath $own
    $ownFileRules = @((Get-Acl -LiteralPath $ownFile).GetAccessRules($true, $false, [System.Security.Principal.SecurityIdentifier]))
    Assert ($ownAcl.AreAccessRulesProtected -and $ownFileRules.Count -eq 1 -and $ownFileRules[0].IdentityReference.Value -eq 'S-1-1-0') 'root-acl-precondition-items-with-rules-of-their-own'
    $a1 = Set-IemRootAcl -Root $root
    Assert $a1.changed 'root-acl-changes-an-open-root'
    $ra = Get-Acl -LiteralPath $root
    $rules = @($ra.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
    Assert ($ra.AreAccessRulesProtected -and $rules.Count -eq 3) 'root-acl-protected-with-three-rules'
    Assert ((Sorted ($rules | ForEach-Object { $_.IdentityReference.Value })) -eq (Sorted @($me.sid, 'S-1-5-18', 'S-1-5-32-544'))) 'root-acl-user-system-administrators'
    Assert (@($rules | Where-Object { [int]$_.FileSystemRights -ne 2032127 -or [int]$_.InheritanceFlags -ne 3 -or $_.IsInherited }).Count -eq 0) 'root-acl-full-control-inherited-below'
    $childAcl = Get-Acl -LiteralPath (Join-Path $root 'bundles\x.txt')
    $childRules = @($childAcl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
    Assert ($childRules.Count -eq 3 -and @($childRules | Where-Object { -not $_.IsInherited -or $_.IdentityReference.Value -eq 'S-1-1-0' }).Count -eq 0) "root-acl-reaches-existing-children ($($childAcl.GetSecurityDescriptorSddlForm('Access')))"
    # Windows' own propagation never takes rules of its own from an item, so
    # these two are always reset by Set-IemRootAcl itself.
    Assert ((@($a1.reset) -contains 'bundles\own') -and (@($a1.reset) -contains 'bundles\own\y.txt')) "root-acl-resets-the-items-with-rules-of-their-own ($(@($a1.reset) -join ', '))"
    foreach ($p in @($own, $ownFile)) {
        $pa = Get-Acl -LiteralPath $p
        $pr = @($pa.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
        Assert (-not $pa.AreAccessRulesProtected -and $pr.Count -eq 3 -and @($pr | Where-Object { -not $_.IsInherited }).Count -eq 0 -and
                (Sorted ($pr | ForEach-Object { $_.IdentityReference.Value })) -eq (Sorted @($me.sid, 'S-1-5-18', 'S-1-5-32-544'))) "root-acl-an-item-with-rules-of-its-own-inherits-only-the-root [$p] ($($pa.GetSecurityDescriptorSddlForm('Access')))"
    }
    $a2 = Set-IemRootAcl -Root $root
    Assert (-not $a2.changed) 'root-acl-second-run-changes-nothing'
    # A junction below the root is refused before anything is written: setting
    # a folder's DACL makes Windows propagate it to what lies below, and whether
    # that goes through a junction is not relied on. Each case would otherwise
    # write: (a) a loosened root, (b) the junction's folder with a rule of its
    # own. Neither the root, the junction's folder nor the folder the junction
    # points to (outside the root) and its file change.
    $outside = Join-Path $base 'outside'
    $outsideFile = Join-Path $outside 'z.txt'
    New-Item -ItemType Directory -Force -Path $outside | Out-Null
    Set-Content -LiteralPath $outsideFile -Value 'z'
    $zAcl = [IO.File]::GetAccessControl($outsideFile)
    $zAcl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($everyone, 'Read', 'Allow')))
    [IO.File]::SetAccessControl($outsideFile, $zAcl)
    function OutsideSddl { return ([IO.Directory]::GetAccessControl($outside).GetSecurityDescriptorSddlForm('Access') + ' ' + [IO.File]::GetAccessControl($outsideFile).GetSecurityDescriptorSddlForm('Access')) }
    function DirSddl($p) { return [IO.Directory]::GetAccessControl($p).GetSecurityDescriptorSddlForm('Access') }
    $outsideBefore = OutsideSddl
    $bundles = Join-Path $root 'bundles'
    $link = Join-Path $bundles 'link'
    # (a) The root loosened as above (set before the junction exists, so the
    # set-up itself passes nothing through it), then the junction.
    $acl = Get-Acl -LiteralPath $root
    $acl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule('Everyone', 'Read', 'ContainerInherit, ObjectInherit', 'None', 'Allow')))
    Set-Acl -LiteralPath $root -AclObject $acl
    $pre = Test-IemDirectoryAcl -Path $root -Rights (Get-IemRootRights -UserSid $me.sid)
    Assert ($pre.Count -gt 0) "root-acl-precondition-a-loosened-root ($($pre -join '; '))"
    New-Item -ItemType Junction -Path $link -Value $outside | Out-Null
    $rootBefore = DirSddl $root
    $bundlesBefore = DirSddl $bundles
    $e = ErrorOf { Set-IemRootAcl -Root $root }
    Assert ($e -like '*junction or a link*' -and $e.Contains('bundles\link')) "root-acl-refuses-a-junction-below-a-loosened-root ($e)"
    Assert ((DirSddl $root) -ceq $rootBefore -and (DirSddl $bundles) -ceq $bundlesBefore) "root-acl-writes-nothing-with-a-junction-below-a-loosened-root ($(DirSddl $root) / $(DirSddl $bundles))"
    Assert ((OutsideSddl) -ceq $outsideBefore) "root-acl-never-writes-through-a-junction-below-a-loosened-root ($(OutsideSddl))"
    [IO.Directory]::Delete($link)
    $r = Set-IemRootAcl -Root $root
    Assert ($r.changed -and @($r.before).Count -gt 0) "root-acl-without-the-junction-closes-the-loosened-root ($(@($r.reset) -join ', '))"
    # (b) The junction's folder with a rule of its own, for that folder alone
    # (so setting it passes nothing on below), then the junction.
    $bAcl = [IO.Directory]::GetAccessControl($bundles)
    $bAcl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($everyone, 'Read', 'None', 'None', 'Allow')))
    [IO.Directory]::SetAccessControl($bundles, $bAcl)
    $pre = Test-IemInheritedItem -Path $bundles -Rights (Get-IemRootRights -UserSid $me.sid)
    Assert ($pre.Count -gt 0) "root-acl-precondition-the-junctions-folder-with-a-rule-of-its-own ($($pre -join '; '))"
    New-Item -ItemType Junction -Path $link -Value $outside | Out-Null
    $rootBefore = DirSddl $root
    $bundlesBefore = DirSddl $bundles
    $e = ErrorOf { Set-IemRootAcl -Root $root }
    Assert ($e -like '*junction or a link*' -and $e.Contains('bundles\link')) "root-acl-refuses-a-junction-in-a-folder-with-a-rule-of-its-own ($e)"
    Assert ((DirSddl $root) -ceq $rootBefore -and (DirSddl $bundles) -ceq $bundlesBefore) "root-acl-writes-nothing-with-a-junction-in-a-folder-with-a-rule-of-its-own ($(DirSddl $root) / $(DirSddl $bundles))"
    Assert ((OutsideSddl) -ceq $outsideBefore) "root-acl-never-writes-through-a-junction-in-a-folder-with-a-rule-of-its-own ($(OutsideSddl))"
    # A reset checks every part from the root down to the item right before it
    # writes (by path): an item reached through a junction, or named outside
    # the root, is refused and nothing outside the root changes.
    $e = ErrorOf { Reset-IemInheritedItem -Root $root -Rel 'bundles\link\z.txt' }
    Assert ($e -like '*junction or a link*' -and $e.Contains('bundles\link')) "root-acl-reset-refuses-an-item-through-a-junction ($e)"
    $e = ErrorOf { Reset-IemInheritedItem -Root $root -Rel '..\outside\z.txt' }
    Assert ($e -like '*not an item below the root*') "root-acl-reset-refuses-an-item-outside-the-root ($e)"
    Assert ((OutsideSddl) -ceq $outsideBefore) "root-acl-reset-never-writes-outside-the-root ($(OutsideSddl))"
    [IO.Directory]::Delete($link)
    $r = Set-IemRootAcl -Root $root
    Assert ((@($r.reset) -join ',') -ceq 'bundles') "root-acl-without-the-junction-resets-its-folder ($(@($r.reset) -join ','))"
    Assert (-not (Set-IemRootAcl -Root $root).changed) 'root-acl-without-the-junction-changes-nothing'

    # ---- firewall rule (disabled here) ----
    $f1 = Add-IemFirewallRule -Name $ruleName -Disabled
    $fr = Get-NetFirewallRule -Name $ruleName
    $pf = $fr | Get-NetFirewallPortFilter
    Assert ($f1.changed -and "$($fr.Direction)" -eq 'Inbound' -and "$($fr.Action)" -eq 'Allow' -and "$($fr.Enabled)" -eq 'False') 'firewall-rule-created-inbound-allow'
    Assert ("$($pf.Protocol)" -eq 'TCP' -and (Sorted $pf.LocalPort) -eq '443,80') 'firewall-rule-tcp-80-and-443'
    Assert ((Sorted ("$($fr.Profile)" -split ',\s*')) -eq 'Domain,Private') 'firewall-rule-private-and-domain-profiles-only'
    Assert (-not (Add-IemFirewallRule -Name $ruleName -Disabled).changed) 'firewall-rule-second-run-changes-nothing'
    Set-NetFirewallRule -Name $ruleName -LocalPort 8080
    $f3 = Add-IemFirewallRule -Name $ruleName -Disabled
    Assert ($f3.changed -and (Sorted ($fr | Get-NetFirewallPortFilter).LocalPort) -eq '443,80') 'firewall-rule-drift-is-repaired'

    # ---- service right ----
    New-Service -Name $svcName -BinaryPathName ('"' + $appExe + '" /c exit 0') -StartupType Manual | Out-Null
    Assert (-not (Test-IemServiceRight -Service $svcName).granted) 'service-right-absent-on-a-new-service'
    $g1 = Grant-IemServiceRight -Service $svcName
    Assert ($g1.changed -and $g1.granted -and $g1.after -ne $g1.before) 'service-right-granted-and-read-back'
    $g2 = Grant-IemServiceRight -Service $svcName
    Assert (-not $g2.changed -and $g2.after -eq $g1.after) 'service-right-second-run-changes-nothing'
    Assert ((Test-IemServiceRight -Service $svcName).granted) 'service-right-test-sees-the-grant'
    Assert (Test-IemServiceGrant -Sddl "D:(A;;RPWPLO;;;$userSid)" -Sid $userSid) 'service-grant-rp-wp-lo'
    Assert (-not (Test-IemServiceGrant -Sddl "D:(A;;RPLO;;;$userSid)" -Sid $userSid)) 'service-grant-needs-stop-too'
    Assert (-not (Test-IemServiceGrant -Sddl "D:(A;;RPWPLO;;;$userSid)(D;;WP;;;$userSid)" -Sid $userSid)) 'service-grant-refuses-a-deny'
    Assert (-not (Test-IemServiceGrant -Sddl "D:(A;;RPWPLO;;;BA)" -Sid $userSid)) 'service-grant-needs-the-users-own-ace'
    # An inherit-only ACE does not apply to the service itself; a deny counts, inherited or not.
    Assert (-not (Test-IemServiceGrant -Sddl "D:(A;CIIO;RPWPLO;;;$userSid)" -Sid $userSid)) 'service-grant-skips-an-inherit-only-ace'
    Assert (-not (Test-IemServiceGrant -Sddl "D:(A;;RPWPLO;;;$userSid)(D;ID;WP;;;$userSid)" -Sid $userSid)) 'service-grant-refuses-an-inherited-deny'

    # ---- bundle sums and Defender exclusions ----
    $sha = '0123456789abcdef0123456789abcdef01234567'
    $keepSha = '2222222222222222222222222222222222222222'
    $oldSha = '1111111111111111111111111111111111111111'
    $bdir = Join-Path $root "bundles\$sha"
    New-Item -ItemType Directory -Force -Path (Join-Path $bdir 'tuning') | Out-Null
    $files = @('iem-engine.exe', 'iemmode.exe', 'hil-v1.ps1', 'tuning\IemTuning.psm1')
    foreach ($n in $files) { Set-Content -LiteralPath (Join-Path $bdir $n) -Value "content of $n" }
    function Write-Sums([string]$dir, [string[]]$extra = @()) {
        $lines = @(Get-ChildItem -LiteralPath $dir -File -Recurse | Where-Object { $_.Name -ne 'SHA256SUMS' } | Sort-Object FullName | ForEach-Object {
            '{0}  {1}' -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $_.FullName.Substring($dir.Length + 1).Replace('\', '/')
        }) + $extra
        [IO.File]::WriteAllText((Join-Path $dir 'SHA256SUMS'), (($lines -join "`n") + "`n"))
    }
    Write-Sums $bdir
    $names = Test-IemBundleSums -Dir $bdir
    Assert ((Sorted $names) -ceq (Sorted @('hil-v1.ps1', 'iem-engine.exe', 'iemmode.exe', 'tuning/IemTuning.psm1'))) 'sums-list-the-bundle-and-its-tuning-folder'
    Set-Content -LiteralPath (Join-Path $bdir 'iemmode.exe') -Value 'tampered'
    Throws { Test-IemBundleSums -Dir $bdir } 'sums-detect-a-changed-file'
    Set-Content -LiteralPath (Join-Path $bdir 'iemmode.exe') -Value 'content of iemmode.exe'
    Set-Content -LiteralPath (Join-Path $bdir 'extra.txt') -Value 'x'
    $e = ErrorOf { Test-IemBundleSums -Dir $bdir }
    Assert ($e -like '*not in SHA256SUMS: extra.txt*') "sums-refuse-an-unlisted-file ($e)"
    Remove-Item -LiteralPath (Join-Path $bdir 'extra.txt')
    New-Item -ItemType Directory -Force -Path (Join-Path $bdir 'other') | Out-Null
    Set-Content -LiteralPath (Join-Path $bdir 'other\x.txt') -Value 'x'
    Write-Sums $bdir
    $e = ErrorOf { Test-IemBundleSums -Dir $bdir }
    Assert ($e -like '*name refused: other/x.txt*') "sums-refuse-a-folder-other-than-tuning ($e)"
    Remove-Item -LiteralPath (Join-Path $bdir 'other') -Recurse -Force
    $h = (Get-FileHash -LiteralPath (Join-Path $bdir 'iemmode.exe')).Hash.ToLowerInvariant()
    foreach ($line in @("$h  ../iemmode.exe", "$h  tuning/..", "$($h.ToUpperInvariant())  iemmode.exe", "$h iemmode.exe", "$h  SHA256SUMS", "$h  a\b.exe")) {
        Write-Sums $bdir @($line)
        Throws { Test-IemBundleSums -Dir $bdir } "sums-refuse-the-line [$line]"
    }
    Write-Sums $bdir @("$h  iemmode.exe")
    $e = ErrorOf { Test-IemBundleSums -Dir $bdir }
    Assert ($e -like '*lists iemmode.exe twice*') "sums-refuse-a-name-listed-twice ($e)"
    Write-Sums $bdir @("$h  gone.exe")
    $e = ErrorOf { Test-IemBundleSums -Dir $bdir }
    Assert ($e -like '*listed in SHA256SUMS but missing: gone.exe*') "sums-refuse-a-missing-file ($e)"
    Write-Sums $bdir

    $pre = (Join-Path $root 'bundles') + '\'
    $want = @("$pre$sha\iem-engine.exe", "$pre$sha\iemmode.exe")
    $current = @("$pre$oldSha\iem-engine.exe", "$pre$keepSha\iem-engine.exe", 'C:\Other\tool.exe', $want[0].ToUpperInvariant(), "$pre$sha\gone.exe")
    $plan = Get-IemExclusionPlan -Root $root -Sha $sha -Want $want -Keep @($keepSha) -Current $current
    Assert ((Sorted $plan.add) -ceq $want[1]) 'exclusions-add-only-what-is-missing-ignoring-case'
    Assert ((Sorted $plan.remove) -ceq (Sorted @("$pre$oldSha\iem-engine.exe", "$pre$sha\gone.exe"))) 'exclusions-remove-unkept-bundles-and-stale-entries-only'
    $plan = Get-IemExclusionPlan -Root $root -Sha $sha -Want $want -Keep @() -Current @()
    Assert ((Sorted $plan.add) -ceq (Sorted $want) -and @($plan.remove).Count -eq 0) 'exclusions-from-nothing'
    $d = Set-IemDefenderExclusion -Root $root -Sha $sha -Keep @($keepSha) -WhatIf
    Assert ($d.whatif -and (Sorted $d.add) -ceq (Sorted $want) -and @($d.remove).Count -eq 0) "defender-whatif-plans-this-bundles-executables ($($d.defender))"
    Throws { Set-IemDefenderExclusion -Root $root -Sha 'ABC' -WhatIf } 'defender-refuses-a-bad-sha'
    Throws { Set-IemDefenderExclusion -Root $root -Sha $sha -Keep @('x') -WhatIf } 'defender-refuses-a-bad-kept-sha'
    Set-Content -LiteralPath (Join-Path $bdir 'iem-engine.exe') -Value 'tampered'
    Throws { Set-IemDefenderExclusion -Root $root -Sha $sha -WhatIf } 'defender-re-verifies-the-bundle'
    Set-Content -LiteralPath (Join-Path $bdir 'iem-engine.exe') -Value 'content of iem-engine.exe'

    # ---- runner (-WhatIf; the token never reaches a command line) ----
    $env:ACTIONS_RUNNER_INPUT_TOKEN = 'SYNTHETICTOKEN0000000000'
    $rw = Register-IemRunner -Dir (Join-Path $base 'runner') -WhatIf
    Assert ($rw.whatif -and $rw.token_present -and -not $rw.configured -and $rw.version -eq '2.337.0') 'runner-whatif-plans-the-pinned-version'
    Assert (($rw.args -join ' ') -ceq '--unattended --url https://github.com/zbynekdrlik/iemmixer-ops --labels iem-pc --work _work --replace') 'runner-config-arguments'
    Assert (($rw.args -join ' ') -notlike '*SYNTHETICTOKEN*' -and -not (Test-Path -LiteralPath (Join-Path $base 'runner'))) 'runner-token-never-on-the-command-line-and-nothing-touched'
    Throws { Register-IemRunner -Dir (Join-Path $base 'runner') -Url 'https://example.org/x/y' -WhatIf } 'runner-refuses-another-host'
    Throws { Register-IemRunner -Dir (Join-Path $base 'runner') -Sha256 'abc' -WhatIf } 'runner-needs-a-pinned-hash'
    Throws { Register-IemRunner -Dir (Join-Path $base 'runner') -Label 'iem pc' -WhatIf } 'runner-refuses-a-bad-label'
    $done = Join-Path $base 'runner-done'
    New-Item -ItemType Directory -Force -Path $done | Out-Null
    [IO.File]::WriteAllText((Join-Path $done '.runner'), '{"agentName":"x","gitHubUrl":"https://github.com/zbynekdrlik/iemmixer-ops"}')
    $rr = Register-IemRunner -Dir $done
    Assert ($rr.configured -and -not $rr.changed) 'runner-already-registered-is-a-no-op'
    Assert (-not (Test-Path -LiteralPath 'Env:\ACTIONS_RUNNER_INPUT_TOKEN')) 'runner-drops-the-token-from-the-environment'
    [IO.File]::WriteAllText((Join-Path $done '.runner'), '{"agentName":"x","gitHubUrl":"https://github.com/someone/else"}')
    Throws { Register-IemRunner -Dir $done } 'runner-registered-elsewhere-refuses'
    $env:ACTIONS_RUNNER_INPUT_TOKEN = ''
    $e = ErrorOf { Register-IemRunner -Dir (Join-Path $base 'runner-no-token') -Zip (Join-Path $base 'no-such.zip') }
    Assert ($e -like '*missing from ACTIONS_RUNNER_INPUT_TOKEN*' -and -not (Test-Path -LiteralPath (Join-Path $base 'runner-no-token'))) "runner-refuses-without-a-token ($e)"

    # ---- tunnel origin (read only) ----
    $cfg = '{"version":3,"config":{"ingress":[{"hostname":"mixer.example.org","service":"http://localhost:80","originRequest":{}},{"service":"http_status:404"}],"warp-routing":{"enabled":false}}}' | ConvertFrom-Json
    $tc = ConvertFrom-IemTunnelConfig -Config $cfg -Metrics 'http://127.0.0.1:20241'
    Assert ($tc.rules.Count -eq 2 -and $tc.rules[0].origin -and $tc.rules[0].loopback -and $tc.rules[0].port -eq 80 -and $tc.rules[0].scheme -eq 'http') 'tunnel-config-loopback-origin'
    Assert (-not $tc.rules[1].origin -and $tc.all_loopback -and $tc.version -eq 3) 'tunnel-config-the-catch-all-is-no-origin'
    Assert ($tc.summary -ceq '2 rule(s); origins: http :80 loopback' -and $tc.summary -notlike '*example*') 'tunnel-summary-names-no-host'
    $remote = ConvertFrom-IemTunnelConfig -Config ('{"config":{"ingress":[{"hostname":"a.example.org","service":"https://10.0.0.10:443"},{"hostname":"b.example.org","service":"http://127.0.0.1:80"},{"service":"http_status:404"}]}}' | ConvertFrom-Json)
    Assert (-not $remote.all_loopback -and $remote.rules[0].port -eq 443 -and -not $remote.rules[0].loopback -and $remote.rules[1].loopback) 'tunnel-config-a-remote-origin'
    $e = ErrorOf { Get-IemTunnelOrigin -Ports @(9) }
    Assert ($e -like '*no tunnel connector answered*') "tunnel-origin-refuses-without-a-connector ($e)"

    # ---- predecessor facts (read only) ----
    $noop = New-ScheduledTaskAction -Execute 'cmd.exe' -Argument '/c exit 0'
    Register-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'test-daily' -Action $noop -Trigger (New-ScheduledTaskTrigger -Daily -At '03:00') | Out-Null
    Register-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'test-logon' -Action $noop -Trigger (New-ScheduledTaskTrigger -AtLogOn) | Out-Null
    $pd = Get-IemPredecessorFacts -ReaperTask '\iemmixer-test\test-daily' -AppExe $appExe
    Assert ($pd.reaper_task.may_fire_in_dev_time -and $pd.reaper_task.triggers[0].kind -eq 'daily' -and $pd.reaper_task.xml -like '*CalendarTrigger*') 'predecessor-a-daily-trigger-may-fire-in-dev-time'
    Assert ($pd.app.sha256 -ceq (Get-FileHash -LiteralPath $appExe -Algorithm SHA256).Hash.ToLowerInvariant() -and $pd.app.file_version) 'predecessor-app-version-and-hash'
    $pl = Get-IemPredecessorFacts -ReaperTask '\iemmixer-test\test-logon' -AppExe $appExe
    Assert (-not $pl.reaper_task.may_fire_in_dev_time -and $pl.reaper_task.triggers[0].kind -eq 'logon') 'predecessor-a-logon-trigger-fires-only-at-logon'
    Disable-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'test-daily' | Out-Null
    Assert (-not (Get-IemPredecessorFacts -ReaperTask '\iemmixer-test\test-daily' -AppExe $appExe).reaper_task.may_fire_in_dev_time) 'predecessor-a-disabled-task-never-fires'
    Throws { Get-IemPredecessorFacts -ReaperTask '\iemmixer-test\no-such-task' -AppExe $appExe } 'predecessor-a-missing-task-refuses'
    $trig = @([pscustomobject]@{ type = 8; enabled = $true }, [pscustomobject]@{ type = 1; enabled = $false })
    Assert (-not (Test-IemTriggersMayFire -Enabled $true -Triggers $trig)) 'triggers-boot-and-a-disabled-time-trigger-never-fire'
    $trig += [pscustomobject]@{ type = 6; enabled = $true }
    Assert (Test-IemTriggersMayFire -Enabled $true -Triggers $trig) 'triggers-an-idle-trigger-may-fire'
    Assert (-not (Test-IemTriggersMayFire -Enabled $true -Triggers @())) 'triggers-none-never-fire'

    # ---- the preference and the elevated tasks' body ----
    New-ItemProperty -LiteralPath $regKey -Name 'Pref' -Value 32 -PropertyType DWord | Out-Null
    $noTuning = Join-Path $base 'no-tuning'
    $lg = Invoke-IemTaskRequest -Kind logon -Root $root -OutDir $eout -TuningDir $noTuning -PrefKey $regKey -PrefName 'Pref' -PrefOriginal '64'
    $pv = Get-IemPref -Key $regKey -Name 'Pref'
    Assert ($lg.ok -and $pv.value -eq 64 -and $pv.kind -eq 'DWord' -and $lg.result.pref.attempts -eq 1 -and $lg.result.tuning -eq 'absent') 'logon-restores-the-preference-keeping-its-kind'
    Assert ((Get-Content -LiteralPath (Join-Path $eout 'logon.result.json') -Raw | ConvertFrom-Json).ok) 'logon-writes-its-result-in-the-admin-only-folder'
    Assert (-not (Test-Path -LiteralPath (Join-Path $root 'guard'))) 'the-elevated-task-writes-nothing-in-the-users-root'
    $r0 = Restore-IemPref -Key $regKey -Name 'Pref' -Original '64'
    Assert ($r0.ok -and $r0.attempts -eq 0) 'pref-at-its-original-is-not-written'
    New-ItemProperty -LiteralPath $regKey -Name 'Text' -Value '32' -PropertyType String | Out-Null
    $rt = Restore-IemPref -Key $regKey -Name 'Text' -Original '064'
    $tv = Get-IemPref -Key $regKey -Name 'Text'
    Assert ($rt.ok -and $tv.raw -ceq '064' -and $tv.kind -eq 'String') 'pref-restores-the-raw-text-of-a-string'
    Throws { Restore-IemPref -Key $regKey -Name 'Pref' -Original '6 4' } 'pref-refuses-a-non-numeric-original'
    Assert ((ConvertTo-IemHkcuPath -Key 'Software\ASIO\Test Card') -ceq 'HKCU:\Software\ASIO\Test Card') 'pref-site-key-is-under-hkcu'
    $nk = Invoke-IemTaskRequest -Kind logon -Root $root -OutDir $eout -TuningDir $noTuning -PrefKey $regKey -PrefName 'NoSuchValue' -PrefOriginal '64'
    Assert (-not $nk.ok -and $nk.error) 'logon-reports-a-missing-value'

    $td = Join-Path $root 'guard\tasks'
    New-Item -ItemType Directory -Force -Path $td | Out-Null
    [IO.File]::WriteAllText((Join-Path $td 'tuning.request.json'), '{"id":"t-1","verb":"state"}')
    $tr = Invoke-IemTaskRequest -Kind tuning -Root $root -OutDir $eout -TuningDir $noTuning
    Assert ($tr.ok -and $tr.id -ceq 't-1' -and $tr.result -eq 'absent') 'tuning-request-is-absent-before-s1c'
    [IO.File]::WriteAllText((Join-Path $td 'tuning.request.json'), '{"id":"t-2","verb":"format"}')
    $tr = Invoke-IemTaskRequest -Kind tuning -Root $root -OutDir $eout -TuningDir $noTuning
    Assert (-not $tr.ok -and $tr.id -ceq 't-2' -and $tr.error -like '*refused*') 'tuning-request-refuses-an-unknown-verb'
    [IO.File]::WriteAllText((Join-Path $td 'tuning.request.json'), '{"id":"t 3; x","verb":"state"}')
    Assert (-not (Invoke-IemTaskRequest -Kind tuning -Root $root -OutDir $eout -TuningDir $noTuning).ok) 'task-request-refuses-a-bad-id'
    [IO.File]::WriteAllText((Join-Path $td 'tuning.request.json'), 'not json at all')
    $tr = Invoke-IemTaskRequest -Kind tuning -Root $root -OutDir $eout -TuningDir $noTuning
    Assert (-not $tr.ok -and $tr.error -ceq 'the request is not JSON') "task-request-refuses-a-non-json-request-without-echoing-it ($($tr.error))"
    [IO.File]::WriteAllText((Join-Path $td 'exclude.request.json'), '{"id":"x-1","sha":"..\\..\\x","keep":[]}')
    $xr = Invoke-IemTaskRequest -Kind exclude -Root $root -OutDir $eout
    Assert (-not $xr.ok -and $xr.error -like '*not a bundle SHA*') 'exclude-request-refuses-a-bad-sha'
    Throws { Invoke-IemTaskRequest -Kind format -Root $root -OutDir $eout } 'task-request-refuses-an-unknown-kind'
    $userOut = Join-Path $base 'user-out'
    New-Item -ItemType Directory -Force -Path $userOut | Out-Null
    $e = ErrorOf { Invoke-IemTaskRequest -Kind tuning -Root $root -OutDir $userOut -TuningDir $noTuning }
    Assert ($e -like '*result folder is refused*' -and -not (Test-Path -LiteralPath (Join-Path $userOut 'tuning.result.json'))) "task-request-never-writes-into-a-user-writable-folder ($e)"

    # A junction on the request path (the redirect of the classic elevated-write
    # attack) is refused, and the result still lands only in the admin-only folder.
    $jroot = Join-Path $base 'jroot'
    $jtarget = Join-Path $base 'jtarget'
    New-Item -ItemType Directory -Force -Path (Join-Path $jroot 'guard'), $jtarget | Out-Null
    [IO.File]::WriteAllText((Join-Path $jtarget 'tuning.request.json'), '{"id":"j-1","verb":"state"}')
    New-Item -ItemType Junction -Path (Join-Path $jroot 'guard\tasks') -Value $jtarget | Out-Null
    Assert ((Test-IemReparsePoint -Path (Join-Path $jroot 'guard\tasks')) -and -not (Test-IemReparsePoint -Path $jtarget) -and -not (Test-IemReparsePoint -Path (Join-Path $base 'no-such-thing'))) 'reparse-points-are-seen-and-folders-are-not'
    $jr = Invoke-IemTaskRequest -Kind tuning -Root $jroot -OutDir $eout -TuningDir $noTuning
    $jres = Get-Content -LiteralPath (Join-Path $eout 'tuning.result.json') -Raw | ConvertFrom-Json
    Assert (-not $jr.ok -and $jr.error -like '*junction or a link*' -and $jres.error -like '*junction or a link*') "task-request-refuses-a-junctioned-tasks-folder ($($jr.error))"
    Assert (@(Get-ChildItem -LiteralPath $jtarget -File).Count -eq 1) 'task-request-writes-nothing-through-the-junction'
    $junctionAsRoot = Join-Path $base 'elevated-link'
    New-Item -ItemType Junction -Path $junctionAsRoot -Value $jtarget | Out-Null
    $e = ErrorOf { Install-IemElevatedFolder -Path $junctionAsRoot -UserSid $me.sid }
    Assert ($e -like '*junction or a link*') "elevated-folder-refuses-a-junction ($e)"
    $foreign = Join-Path $base 'foreign'
    New-Item -ItemType Directory -Force -Path $foreign | Out-Null
    $ds = New-Object System.Security.AccessControl.DirectorySecurity
    $ds.SetOwner((New-Object System.Security.Principal.SecurityIdentifier $me.sid))
    [IO.Directory]::SetAccessControl($foreign, $ds)
    $e = ErrorOf { Install-IemElevatedFolder -Path $foreign -UserSid $me.sid }
    Assert ($e -like '*is owned by*refused*') "elevated-folder-refuses-a-folder-someone-else-made ($e)"

    # S1c's tuning module is imported only from an admin-owned, admin-only folder.
    $fakeTuning = @'
function Get-IemTuningState { param([string]$ProfilePath) return 'fake-state' }
function Enter-IemTuningMode { param([string]$ProfilePath) return 'fake-enter' }
function Exit-IemTuningMode { param([string]$ProfilePath) return 'fake-exit' }
function Invoke-IemTuningApply { param([string]$ProfilePath, [int]$Tier) return ('fake-apply-{0}' -f $Tier) }
'@
    $tmod = Join-Path $etuning 'IemTuning.psm1'
    $tprof = Join-Path $etuning 'profile.json'
    function Write-FakeTuning {
        foreach ($p in @($tmod, $tprof)) { if (Test-Path -LiteralPath $p) { Remove-Item -LiteralPath $p -Force } }
        [IO.File]::WriteAllText($tmod, $fakeTuning)
        [IO.File]::WriteAllText($tprof, '{}')
        foreach ($p in @($tmod, $tprof)) { Set-IemAdminsOwner -Path $p }
    }
    Write-FakeTuning
    Assert ((Invoke-IemTuningVerb -Verb 'state' -TuningDir $etuning) -ceq 'fake-state') 'tuning-imports-an-admin-only-module'
    Assert ((Invoke-IemTuningVerb -Verb 'apply-tier2' -TuningDir $etuning) -ceq 'fake-apply-2') 'tuning-apply-tier2-passes-tier-2'
    [IO.File]::WriteAllText((Join-Path $td 'tuning.request.json'), '{"id":"t-5","verb":"enter"}')
    $tr = Invoke-IemTaskRequest -Kind tuning -Root $root -OutDir $eout -TuningDir $etuning
    Assert ($tr.ok -and $tr.result -ceq 'fake-enter') 'tuning-request-runs-the-verified-module'
    foreach ($p in @($tmod, $tprof)) {
        # The user may change it: refused.
        $fs = [IO.File]::GetAccessControl($p)
        $fs.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule((New-Object System.Security.Principal.SecurityIdentifier $me.sid), 'Modify', 'Allow')))
        [IO.File]::SetAccessControl($p, $fs)
        $e = ErrorOf { Invoke-IemTuningVerb -Verb 'state' -TuningDir $etuning }
        Assert ($e -like '*tuning module refused*') "tuning-refuses-a-user-writable-file [$p] ($e)"
        Write-FakeTuning
        # Owned by the user (who could change its DACL): refused.
        $fo = New-Object System.Security.AccessControl.FileSecurity
        $fo.SetOwner((New-Object System.Security.Principal.SecurityIdentifier $me.sid))
        [IO.File]::SetAccessControl($p, $fo)
        $e = ErrorOf { Invoke-IemTuningVerb -Verb 'state' -TuningDir $etuning }
        Assert ($e -like "*tuning module refused*owned by $($me.sid)*") "tuning-refuses-a-file-owned-by-the-user [$p] ($e)"
        $tr = Invoke-IemTaskRequest -Kind tuning -Root $root -OutDir $eout -TuningDir $etuning
        Assert (-not $tr.ok -and $tr.error -like '*tuning module refused*') "tuning-request-refuses-an-unverified-module [$p]"
        Write-FakeTuning
    }
    # The tuning folder itself: a rule that lets the user add files, then a user owner.
    $dsec = [IO.Directory]::GetAccessControl($etuning)
    $dsec.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule((New-Object System.Security.Principal.SecurityIdentifier $me.sid), 'Modify', 'ContainerInherit, ObjectInherit', 'None', 'Allow')))
    [IO.Directory]::SetAccessControl($etuning, $dsec)
    $e = ErrorOf { Invoke-IemTuningVerb -Verb 'state' -TuningDir $etuning }
    Assert ($e -like '*tuning module refused*') "tuning-refuses-a-user-writable-folder ($e)"
    [IO.Directory]::SetAccessControl($etuning, (New-IemElevatedSecurity -UserSid $me.sid))
    Write-FakeTuning
    Assert ((Invoke-IemTuningVerb -Verb 'state' -TuningDir $etuning) -ceq 'fake-state') 'tuning-folder-restored'
    $do = New-Object System.Security.AccessControl.DirectorySecurity
    $do.SetOwner((New-Object System.Security.Principal.SecurityIdentifier $me.sid))
    [IO.Directory]::SetAccessControl($etuning, $do)
    $e = ErrorOf { Invoke-IemTuningVerb -Verb 'state' -TuningDir $etuning }
    Assert ($e -like "*tuning module refused*owned by $($me.sid)*") "tuning-refuses-a-folder-owned-by-the-user ($e)"
    [IO.Directory]::SetAccessControl($etuning, (New-IemElevatedSecurity -UserSid $me.sid))
    Throws { Invoke-IemTuningVerb -Verb 'state' -TuningDir 'relative\tuning' } 'tuning-refuses-a-relative-folder'
    foreach ($p in @($tmod, $tprof)) { Remove-Item -LiteralPath $p -Force }

    # The generated entry, as the task runs it (its results in tasks\out).
    [IO.File]::WriteAllText((Join-Path $td 'tuning.request.json'), '{"id":"t-4","verb":"exit"}')
    & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $entry -Kind tuning -Root $root -TuningDir $noTuning | Out-Null
    $code = $LASTEXITCODE
    $res = Get-Content -LiteralPath (Join-Path $eout 'tuning.result.json') -Raw | ConvertFrom-Json
    Assert ($code -eq 0 -and $res.ok -and $res.id -ceq 't-4') "elevated-entry-runs-a-request (exit $code)"
    Assert (-not (Test-Path -LiteralPath (Join-Path $td 'tuning.result.json'))) 'elevated-entry-writes-no-result-into-the-users-root'

    # ---- bootstrap state (read only) ----
    $bs = Get-IemBootstrapState -Root $root -Module 'iemmixer-no-such-module.dll' -PrefKey $regKey -PrefName 'Pref' -AppImage 'iemmixer-no-such-app.exe' -Folder $folder -FirewallRule $ruleName
    Assert ($bs.holders.Count -eq 0 -and $bs.app -eq 0 -and $bs.reaper -eq 0 -and $bs.pref.value -eq 64) 'bootstrap-state-holders-processes-and-preference'
    Assert (@($bs.tasks | Where-Object { -not $_.exists -or -not $_.sddl_ok }).Count -eq 0 -and @($bs.tasks).Count -eq 7) 'bootstrap-state-our-tasks-with-their-descriptors'
    Assert ($bs.root.acl_ok -and $bs.firewall.present -and -not $bs.firewall.ok) "bootstrap-state-root-and-the-disabled-test-rule ($(@($bs.root.problems) -join '; '))"
    # An item below the root with a rule of its own: the state sees it, and the
    # next Set-IemRootAcl resets that item only.
    $loose = Join-Path $root 'bundles\x.txt'
    $looseAcl = [IO.File]::GetAccessControl($loose)
    $looseAcl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($everyone, 'Read', 'Allow')))
    [IO.File]::SetAccessControl($loose, $looseAcl)
    $bs = Get-IemBootstrapState -Root $root -Module 'iemmixer-no-such-module.dll' -PrefKey $regKey -PrefName 'Pref' -AppImage 'iemmixer-no-such-app.exe' -Folder $folder -FirewallRule $ruleName
    Assert (-not $bs.root.acl_ok -and @($bs.root.problems | Where-Object { $_ -like '*x.txt*S-1-1-0*' }).Count -eq 1) "bootstrap-state-sees-an-item-below-the-root-with-a-rule-of-its-own ($(@($bs.root.problems) -join '; '))"
    $fix = Set-IemRootAcl -Root $root
    Assert ($fix.changed -and (@($fix.reset) -join ',') -ceq 'bundles\x.txt') "root-acl-resets-only-the-item-that-differs ($(@($fix.reset) -join ','))"
    Assert ((Get-IemModuleHolders -Module 'kernel32.dll').Count -gt 1) 'module-holders-lists-processes'
    Throws { Get-IemModuleHolders -Module 'a|b.dll' } 'module-holders-refuse-a-bad-name'

    # ---- HIL helpers (pure) ----
    $S = '0123456789abcdef0123456789abcdef01234567'
    Assert ((Test-IemHilVersion -Sha $S -Version ([pscustomobject]@{ git_hash = '0123456' })).ok) 'hil-version-a-short-hash-prefix'
    Assert ((Test-IemHilVersion -Sha $S -Version ([pscustomobject]@{ git_hash = $S })).ok) 'hil-version-the-full-sha'
    foreach ($v in @('012345', '1123456', '0123456789ABCDEF', '')) {
        Assert (-not (Test-IemHilVersion -Sha $S -Version ([pscustomobject]@{ git_hash = $v })).ok) "hil-version-refuses [$v]"
    }
    Assert (-not (Test-IemHilVersion -Sha $S -Version ([pscustomobject]@{ version = '2.0.0' })).ok) 'hil-version-refuses-a-missing-hash'
    Assert (-not (Test-IemHilVersion -Sha $S -Version $null).ok) 'hil-version-refuses-no-answer'

    function Eng($cb, $missed, $resets, $frames = 32, $parked = $false, $faulted = $false) {
        [pscustomobject]@{ frames = $frames; callbacks = $cb; missed = $missed; resets = $resets; parked = $parked; faulted = $faulted }
    }
    $c = Test-IemHilCard -Before (Eng 1000 2 1) -After (Eng 343000 2 1) -Seconds 120
    Assert ($c.ok -and $c.numbers.callbacks -eq 342000 -and $c.numbers.missed -eq 0 -and $c.numbers.resets -eq 0) 'hil-card-passes-at-the-floor'
    Assert (-not (Test-IemHilCard -Before (Eng 1000 2 1) -After (Eng 342999 2 1) -Seconds 120).ok) 'hil-card-refuses-one-below-the-floor'
    Assert (-not (Test-IemHilCard -Before (Eng 0 0 0) -After (Eng 360000 1 0) -Seconds 120).ok) 'hil-card-refuses-one-missed-period'
    Assert (-not (Test-IemHilCard -Before (Eng 0 0 0) -After (Eng 360000 0 1) -Seconds 120).ok) 'hil-card-refuses-one-reset'
    Assert (-not (Test-IemHilCard -Before (Eng 0 0 0) -After (Eng 360000 0 0 64) -Seconds 120).ok) 'hil-card-refuses-a-measured-64'
    Assert (-not (Test-IemHilCard -Before (Eng 0 0 0) -After (Eng 360000 0 0 32 $true) -Seconds 120).ok) 'hil-card-refuses-parked'
    Assert (-not (Test-IemHilCard -Before (Eng 0 0 0) -After (Eng 360000 0 0 32 $false $true) -Seconds 120).ok) 'hil-card-refuses-faulted'
    $gap = Test-IemHilCard -Before (Eng 0 0 0) -After ([pscustomobject]@{ frames = 32; callbacks = 360000 }) -Seconds 120
    Assert (-not $gap.ok -and $gap.detail -like "*lacks 'missed'*") 'hil-card-refuses-a-status-without-its-fields'
    Assert ((Test-IemHilCard -Before $null -After (Eng 0 0 0) -Seconds 1).detail -like '*no engine status*') 'hil-card-refuses-no-engine-status'
    Assert ((Test-IemHilReopen -Before (Eng 100 0 2) -After (Eng 200 0 3)).ok) 'hil-reopen-one-reset'
    Assert (-not (Test-IemHilReopen -Before (Eng 100 0 2) -After (Eng 200 0 2)).ok) 'hil-reopen-refuses-no-reset'
    Assert (-not (Test-IemHilReopen -Before (Eng 100 0 2) -After (Eng 200 0 4)).ok) 'hil-reopen-refuses-two-resets'
    Assert (-not (Test-IemHilReopen -Before (Eng 100 0 2) -After (Eng 100 0 3)).ok) 'hil-reopen-refuses-a-stalled-card'
    Assert (-not (Test-IemHilReopen -Before (Eng 100 0 2) -After (Eng 200 0 3 64)).ok) 'hil-reopen-refuses-a-measured-64'
    function EngP($cb, $spawns, $last, $frames = 32, $parked = $false, $faulted = $false) {
        [pscustomobject]@{ frames = $frames; callbacks = $cb; missed = 0; resets = 0; parked = $parked; faulted = $faulted; spawns = $spawns; last_exit = $last }
    }
    $p0 = EngP 5000 1 $null
    $pOk = Test-IemHilPanic -Before $p0 -Respawned (EngP 10 2 70) -Later (EngP 11 2 70)
    Assert ($pOk.ok -and $pOk.numbers.spawns -eq 1 -and $pOk.numbers.callbacks -eq 1) 'hil-panic-exit-70-one-respawn-streaming'
    Assert (-not (Test-IemHilPanic -Before $p0 -Respawned (EngP 10 2 70) -Later (EngP 10 2 70)).ok) 'hil-panic-refuses-a-new-engine-that-does-not-stream'
    Assert (-not (Test-IemHilPanic -Before $p0 -Respawned (EngP 10 2 1) -Later (EngP 6010 2 1)).ok) 'hil-panic-refuses-another-exit-code'
    Assert (-not (Test-IemHilPanic -Before $p0 -Respawned (EngP 10 2 $null) -Later (EngP 6010 2 $null)).ok) 'hil-panic-refuses-no-exit-code'
    Assert (-not (Test-IemHilPanic -Before $p0 -Respawned (EngP 10 3 70) -Later (EngP 6010 3 70)).ok) 'hil-panic-refuses-two-respawns'
    Assert (-not (Test-IemHilPanic -Before $p0 -Respawned (EngP 10 2 70) -Later (EngP 6010 2 70 64)).ok) 'hil-panic-refuses-a-measured-64'
    Assert (-not (Test-IemHilPanic -Before $p0 -Respawned (EngP 10 2 70) -Later (EngP 6010 2 70 32 $true)).ok) 'hil-panic-refuses-parked'
    Assert (-not (Test-IemHilPanic -Before $p0 -Respawned (EngP 10 2 70) -Later (EngP 6010 2 70 32 $false $true)).ok) 'hil-panic-refuses-faulted'
    $pn = Test-IemHilPanic -Before $p0 -Respawned $null -Later $null
    Assert (-not $pn.ok -and $pn.detail -ceq 'no new engine within the wait') 'hil-panic-refuses-no-respawn'
    $pg = Test-IemHilPanic -Before ([pscustomobject]@{ frames = 32 }) -Respawned (EngP 10 2 70) -Later (EngP 6010 2 70)
    Assert (-not $pg.ok -and $pg.detail -like "*lacks 'spawns'*") 'hil-panic-refuses-a-status-without-spawns'
    # What HIL v1's waits look for: the guard shows no engine until its hello and first
    # Status, and that first Status may come before the card streams (frames 0).
    function EngU($cb, $frames = 32, $resets = 2, $build = $S) {
        [pscustomobject]@{ build = $build; frames = $frames; callbacks = $cb; resets = $resets }
    }
    Assert (Test-IemHilEngineUp -Engine (EngU 3000)) 'hil-engine-up-streaming-at-32'
    Assert (-not (Test-IemHilEngineUp -Engine $null)) 'hil-engine-up-refuses-no-engine'
    Assert (-not (Test-IemHilEngineUp -Engine (EngU 0 0 0))) 'hil-engine-up-refuses-the-first-status-before-the-card'
    Assert (-not (Test-IemHilEngineUp -Engine (EngU 3000 64))) 'hil-engine-up-refuses-a-measured-64'
    Assert (-not (Test-IemHilEngineUp -Engine ([pscustomobject]@{ frames = 32 }))) 'hil-engine-up-refuses-a-status-without-callbacks'
    Assert (Test-IemHilEngineUp -Engine (EngU 3000) -Sha $S) 'hil-engine-up-names-the-bundle'
    Assert (-not (Test-IemHilEngineUp -Engine (EngU 3000 32 2 ('f' + $S.Substring(1))) -Sha $S)) 'hil-engine-up-refuses-another-build'
    Assert (-not (Test-IemHilEngineUp -Engine (EngU 3000 32 2 '') -Sha $S)) 'hil-engine-up-refuses-an-empty-build'
    Assert (Test-IemHilEngineUp -Engine (EngU 3000) -Callbacks 2999) 'hil-engine-up-callbacks-above'
    Assert (-not (Test-IemHilEngineUp -Engine (EngU 3000) -Callbacks 3000)) 'hil-engine-up-refuses-callbacks-not-above'
    Assert (Test-IemHilEngineUp -Engine (EngU 3000) -Resets 1) 'hil-engine-up-resets-above'
    Assert (-not (Test-IemHilEngineUp -Engine (EngU 3000) -Resets 2)) 'hil-engine-up-refuses-resets-not-above'
    Assert (-not (Test-IemHilEngineUp -Engine ([pscustomobject]@{ frames = 32; callbacks = 3000 }) -Resets 0)) 'hil-engine-up-refuses-a-status-without-resets'
    # The test signal on HIL's spare outputs (engine.hil: each spare card output and its
    # peak since the engine's previous Status; #9, 2026-09-28).
    function EngH([double[]]$peaks) {
        $outs = @()
        $tx = 94
        foreach ($p in $peaks) { $outs += [pscustomobject]@{ tx = $tx; peak = $p }; $tx++ }
        [pscustomobject]@{ frames = 32; callbacks = 3000; faulted = $false; hil = $outs }
    }
    $lvl = [math]::Pow(10, -30 / 20)
    Assert (Test-IemHilSilent -Engine (EngH @(0, 0))) 'hil-silent-every-spare-output-at-zero'
    Assert (-not (Test-IemHilSilent -Engine (EngH @(0, 0.001)))) 'hil-silent-refuses-one-that-sounds'
    Assert (-not (Test-IemHilSilent -Engine (EngH @()))) 'hil-silent-refuses-no-spare-output'
    Assert (-not (Test-IemHilSilent -Engine (EngU 3000))) 'hil-silent-refuses-a-status-without-hil'
    Assert (-not (Test-IemHilSilent -Engine $null)) 'hil-silent-refuses-no-engine'
    # A status that carried the signal on some spare output: HIL v1's silence wait takes a
    # silent status as "after" only once one of these was read.
    Assert (Test-IemHilHeard -Engine (EngH @(0, 0.001))) 'hil-heard-one-spare-output-that-sounds'
    Assert (Test-IemHilHeard -Engine (EngH @($lvl, $lvl))) 'hil-heard-every-spare-output-at-the-level'
    Assert (-not (Test-IemHilHeard -Engine (EngH @(0, 0)))) 'hil-heard-refuses-silence'
    Assert (-not (Test-IemHilHeard -Engine (EngH @()))) 'hil-heard-refuses-no-spare-output'
    Assert (-not (Test-IemHilHeard -Engine (EngU 3000))) 'hil-heard-refuses-a-status-without-hil'
    Assert (-not (Test-IemHilHeard -Engine $null)) 'hil-heard-refuses-no-engine'
    $quietAfter = EngH @(0, 0)
    $sg = Test-IemHilSignal -During @((EngH @(0, 0)), (EngH @($lvl, ($lvl * 0.99))), $null) -After $quietAfter -Dbfs (-30)
    Assert ($sg.ok -and $sg.numbers.outputs -eq 2 -and $sg.numbers.after -eq 0 -and [math]::Abs($sg.numbers.highest_dbfs + 30) -lt 1e-9) "hil-signal-heard-at-the-level-then-silent ($($sg.detail))"
    Assert ($sg.detail -ceq '2 spare output(s) at -30.1 dBFS (asked -30.0), silent after the TTL') "hil-signal-detail ($($sg.detail))"
    $sg = Test-IemHilSignal -During @((EngH @($lvl, 0))) -After $quietAfter -Dbfs (-30)
    Assert (-not $sg.ok -and $sg.detail -like '*peaked at -150.0 dBFS*') "hil-signal-refuses-a-spare-output-never-heard ($($sg.detail))"
    foreach ($p in @(($lvl * 0.9), ($lvl * 1.1), 0.1)) {
        $sg = Test-IemHilSignal -During @((EngH @($p, $p))) -After $quietAfter -Dbfs (-30)
        Assert (-not $sg.ok) "hil-signal-refuses-another-level [$p]"
    }
    $sg = Test-IemHilSignal -During @((EngH @($lvl, $lvl))) -After (EngH @(0, $lvl)) -Dbfs (-30)
    Assert (-not $sg.ok -and $sg.numbers.after -eq 1 -and $sg.detail -like '*1 spare output(s) still sound after the TTL*') "hil-signal-refuses-one-still-sounding ($($sg.detail))"
    Assert (-not (Test-IemHilSignal -During @() -After $quietAfter -Dbfs (-30)).ok) 'hil-signal-refuses-no-status-during-the-ttl'
    Assert ((Test-IemHilSignal -During @((EngH @($lvl))) -After (EngU 3000) -Dbfs (-30)).detail -ceq "the engine status lacks 'hil'") 'hil-signal-refuses-a-status-without-hil'
    Assert ((Test-IemHilSignal -During @() -After (EngH @()) -Dbfs (-30)).detail -ceq 'the engine opened no HIL output ([guard] hil_tx)') 'hil-signal-refuses-an-engine-without-spare-outputs'

    $ok1 = New-IemHilCheck -Name 'a' -Ok $true
    $bad1 = New-IemHilCheck -Name 'b' -Ok $false -Detail 'no'
    Assert ((Get-IemHilConclusion -Checks @($ok1, $ok1)) -ceq 'success') 'hil-conclusion-success'
    Assert ((Get-IemHilConclusion -Checks @($ok1, $bad1)) -ceq 'failure') 'hil-conclusion-a-failed-check-fails'
    Assert ((Get-IemHilConclusion -Checks @()) -ceq 'failure') 'hil-conclusion-no-check-fails'
    Assert ((Get-IemHilConclusion -Checks @($ok1) -Cancelled) -ceq 'cancelled') 'hil-conclusion-cancelled-wins'
    Assert ((Get-IemHilSummary -Conclusion 'failure' -Checks @($ok1, $bad1)) -ceq 'HIL v1 failure: b (1 of 2 ok)') 'hil-summary-names-the-failed-checks-only'
    Assert ((Get-IemHilSummary -Conclusion 'success' -Checks @($ok1, $ok1)) -ceq 'HIL v1 success: 2 checks ok') 'hil-summary-success'
    # The summary is public (hil/iem-pc on the public repo, P6): a cancelled
    # job names a fixed reason, never the guard's free text.
    Assert ((Get-IemHilSummary -Conclusion 'cancelled' -Reason 'left-dev') -ceq 'HIL v1 cancelled: the guard left dev') 'hil-summary-cancelled-left-dev-is-a-fixed-phrase'
    Assert ((Get-IemHilSummary -Conclusion 'cancelled' -Reason 'not-free') -ceq 'HIL v1 cancelled: the PC was not free (job-begin refused)') 'hil-summary-cancelled-not-free-is-a-fixed-phrase'
    Throws { Get-IemHilSummary -Conclusion 'cancelled' -Reason 'input mic7 refused in C:\Users\member1' } 'hil-summary-cancelled-takes-a-reason-code-never-text'
    Assert ((Get-IemHilSummary -Conclusion 'failure' -Checks @($ok1, $bad1) -Reason 'left-dev') -ceq 'HIL v1 failure: b (1 of 2 ok)') 'hil-summary-a-reason-never-changes-a-failure'

    Assert ((ConvertFrom-IemReply -Text '{"ok":true,"mode":"dev"}').mode -ceq 'dev') 'reply-compact'
    Assert ((ConvertFrom-IemReply -Text "{`n  `"ok`": false,`n  `"mode`": `"event`"`n}").mode -ceq 'event') 'reply-indented'
    Assert ((ConvertFrom-IemReply -Text "a log line`n{`"ok`":true,`"mode`":`"dev`"}").ok -eq $true) 'reply-after-a-log-line'
    foreach ($t in @('', '   ', '[1,2]', '42', 'not json')) { Assert ($null -eq (ConvertFrom-IemReply -Text $t)) "reply-refuses [$t]" }
    Assert (-not (Test-IemHilSwitchStarted -Reply ([pscustomobject]@{ mode = 'dev'; switching = $null }))) 'switch-dev-idle-is-not-a-switch'
    Assert (Test-IemHilSwitchStarted -Reply ([pscustomobject]@{ mode = 'event'; switching = $null })) 'switch-event-mode'
    Assert (Test-IemHilSwitchStarted -Reply ([pscustomobject]@{ mode = 'dev'; switching = [pscustomobject]@{ to = 'event' } })) 'switch-running'
    Assert (-not (Test-IemHilSwitchStarted -Reply $null)) 'switch-unknown-without-a-reply'
    $hilDir = Join-Path $base "bundles\$S"
    Assert ((Test-IemHilInputs -Sha $S -Branch 'dev' -JobRun '4242' -Out 'r.json' -ScriptDir $hilDir).Count -eq 0) 'hil-inputs-valid'
    Assert ((Test-IemHilInputs -Sha $S -Branch 'feature' -JobRun '42a' -Out '' -ScriptDir (Join-Path $base 'elsewhere')).Count -eq 4) 'hil-inputs-branch-run-out-and-place'
    Assert ((Test-IemHilInputs -Sha $S.ToUpperInvariant() -Branch 'main' -JobRun '1' -Out 'r.json' -ScriptDir $hilDir).Count -eq 2) 'hil-inputs-an-uppercase-sha-and-its-folder'
    $hi = @{ Sha = $S; Branch = 'dev'; JobRun = '1'; Out = 'r.json'; ScriptDir = $hilDir }
    Assert ((Test-IemHilInputs @hi -TestDbfs (-20) -TestTtl 60).Count -eq 0) 'hil-inputs-the-test-signal-at-its-limits'
    Assert ((Test-IemHilInputs @hi -TestDbfs (-120) -TestTtl 0.001).Count -eq 0) 'hil-inputs-a-quiet-short-test-signal'
    foreach ($db in @(-19.9, 0, 6, [double]::NaN, [double]::PositiveInfinity, [double]::NegativeInfinity)) {
        $bad = Test-IemHilInputs @hi -TestDbfs $db -TestTtl 10
        Assert ($bad.Count -eq 1 -and $bad[0] -like 'TestDbfs:*') "hil-inputs-refuse-the-level [$db]"
    }
    foreach ($ttl in @(0, -1, 60.001, [double]::NaN)) {
        $bad = Test-IemHilInputs @hi -TestDbfs (-30) -TestTtl $ttl
        Assert ($bad.Count -eq 1 -and $bad[0] -like 'TestTtl:*') "hil-inputs-refuse-the-ttl [$ttl]"
    }
    $m = Invoke-IemMode -Exe (Join-Path $base 'no-such-iemmode.exe') -Arguments @('status')
    Assert ($null -eq $m.exit -and $null -eq $m.reply -and -not (Test-IemModeOk -Result $m)) 'iemmode-that-does-not-start'

    # ---- hil-v1.ps1 end to end, against a stand-in for iemmode ----
    # The bundle layout (bundles\<sha>\ with the module next to the script); the
    # server is unreachable here (port 9), so the address checks fail.
    $hb = Join-Path $base 'hil'
    $hdir = Join-Path $hb "bundles\$S"
    New-Item -ItemType Directory -Force -Path $hdir | Out-Null
    Copy-Item -LiteralPath (Join-Path $here 'hil-v1.ps1'), (Join-Path $here 'IemPc.psm1') -Destination $hdir
    $fake = Join-Path $hb 'fake-iemmode.ps1'
    $fakeText = @'
# A stand-in for iemmode.exe (Test-IemPc.ps1): replies as scenario.json says and logs every call.
$sc = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'scenario.json')) | ConvertFrom-Json
$log = Join-Path $PSScriptRoot 'calls.log'
Add-Content -LiteralPath $log -Value ($args -join ' ')
$cmd = [string]$args[0]
$lines = @(Get-Content -LiteralPath $log)
$n = $lines.Count
if (@($sc.silent) -contains $cmd) { exit 4 }
# `cold` (optional): the guard while an engine comes up. After activate or inject-fault
# (a new engine) that many statuses show no engine, then as many show its first Status
# (frames 0, no callbacks); after force-reopen that many still show the old reset count.
$cold = 0
if ($null -ne $sc.PSObject.Properties['cold']) { $cold = [int]$sc.cold }
# `heard` (optional, default 1): after test-signal that many statuses show both spare
# outputs (engine.hil) at the asked level, the call's third argument, from the status
# `heard_from` (optional, default 1) on; the others silence. The engine's Status carries
# the peaks since its previous one, so a short TTL's signal may show only in a status
# that arrives after the TTL (heard_from above 1).
$heard = 1
if ($null -ne $sc.PSObject.Properties['heard']) { $heard = [int]$sc.heard }
$heardFrom = 1
if ($null -ne $sc.PSObject.Properties['heard_from']) { $heardFrom = [int]$sc.heard_from }
function Get-StatusesSince([string[]]$Marks) {
    # The status calls (this one included) after the last call named in $Marks; -1 without one.
    $count = 0
    for ($i = $lines.Count - 1; $i -ge 0; $i--) {
        $l = [string]$lines[$i]
        foreach ($m in $Marks) { if ($l -ceq $m -or $l.StartsWith($m + ' ')) { return $count } }
        if ($l -ceq 'status') { $count++ }
    }
    return -1
}
$reopens = @($lines | Where-Object { $_ -eq 'force-reopen' }).Count
$mode = 'dev'
if ($sc.event_after -gt 0 -and $n -gt $sc.event_after) { $mode = 'event' }
$ok = -not (@($sc.refuse) -contains $cmd)
$reply = [ordered]@{ ok = $ok; mode = $mode; switching = $null; alarms = @(); detail = ('fake ' + $cmd) }
$faults = @($lines | Where-Object { $_ -eq 'inject-fault' }).Count
if ($cmd -eq 'status') {
    $started = Get-StatusesSince @('activate', 'inject-fault')
    $reopened = Get-StatusesSince @('force-reopen')
    if ($reopened -ge 0 -and $reopened -le $cold) { $reopens-- }
    $last = $null
    if ($faults -gt 0) { $last = 70 }
    $frames = 32
    $callbacks = 3000 * $n
    if ($started -ge 0 -and $started -le 2 * $cold) { $frames = 0; $callbacks = 0 }
    $peak = 0.0
    $signals = @($lines | Where-Object { $_ -like 'test-signal *' })
    $sinceSignal = Get-StatusesSince @('test-signal')
    if ($signals.Count -gt 0 -and $sinceSignal -ge $heardFrom -and $sinceSignal -lt $heardFrom + $heard) {
        $asked = [double]::Parse(([string]$signals[$signals.Count - 1]).Split(' ')[2], [Globalization.CultureInfo]::InvariantCulture)
        $peak = [math]::Pow(10, $asked / 20)
    }
    if ($started -lt 0 -or $started -gt $cold) {
        $reply['engine'] = [ordered]@{ build = $sc.sha; frames = $frames; callbacks = $callbacks; missed = 0; resets = $reopens
                                       parked = $false; faulted = $false; pipe_private = $true; spawns = (1 + $faults); last_exit = $last
                                       hil = @([ordered]@{ tx = 94; peak = $peak }, [ordered]@{ tx = 95; peak = $peak }) }
    }
}
Write-Output (ConvertTo-Json -InputObject $reply -Depth 5 -Compress)
if ($ok) { exit 0 }
exit 1
'@
    [IO.File]::WriteAllText($fake, $fakeText)
    function Invoke-HilRun([string]$scenario, [string]$branch = 'dev', [string]$ttl = '0.2', [string[]]$more = @()) {
        [IO.File]::WriteAllText((Join-Path $hb 'scenario.json'), $scenario)
        foreach ($f in @('calls.log', 'result.json')) { Remove-Item -LiteralPath (Join-Path $hb $f) -ErrorAction SilentlyContinue }
        & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File (Join-Path $hdir 'hil-v1.ps1') -Sha $S -Branch $branch `
            -JobRun 4242 -Out (Join-Path $hb 'result.json') -Iemmode $fake -Local 'http://127.0.0.1:9' -CardSeconds 1 -TestTtl $ttl @more | Out-Null
        $code = $LASTEXITCODE
        $calls = @()
        if (Test-Path -LiteralPath (Join-Path $hb 'calls.log')) { $calls = @(Get-Content -LiteralPath (Join-Path $hb 'calls.log')) }
        $res = Get-Content -LiteralPath (Join-Path $hb 'result.json') -Raw | ConvertFrom-Json
        return [pscustomobject]@{ exit = $code; result = $res; calls = $calls }
    }
    function CheckOk($res, [string]$name) { return @($res.checks | Where-Object { $_.name -eq $name -and $_.ok }).Count -eq 1 }
    function CheckFailed($res, [string]$name) { return @($res.checks | Where-Object { $_.name -eq $name -and -not $_.ok }).Count -eq 1 }
    function CheckDetail($res, [string]$name) { return (@($res.checks | Where-Object { $_.name -eq $name }) | ForEach-Object { $_.detail }) -join ' | ' }

    $h1 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":0}')
    $calls = $h1.calls
    Assert ($h1.exit -eq 1 -and $h1.result.conclusion -ceq 'failure' -and $h1.result.sha -ceq $S -and $h1.result.job_run -ceq '4242') "hil-run-with-the-server-down-fails (exit $($h1.exit))"
    Assert ($calls[0] -ceq 'job-begin 4242' -and $calls[1] -ceq "activate $S") "hil-run-begins-the-job-then-activates ($($calls -join ' | '))"
    Assert ($calls -contains 'test-signal mic1 -30 0.2' -and $calls -contains 'force-reopen' -and $calls -contains 'inject-fault' -and $calls -contains 'alarm-test') 'hil-run-drives-the-signal-reopen-fault-and-alarm'
    Assert ($calls[$calls.Count - 2] -ceq 'job-end 4242' -and $calls[$calls.Count - 1] -like "report $S red HIL v1 failure: *") 'hil-run-ends-the-job-then-reports-red'
    foreach ($n in @('activate', 'engine-build', 'card', 'pipes', 'test-signal', 'reopen', 'panic', 'alarm-push')) { Assert (CheckOk $h1.result $n) "hil-run-check-$n-passes" }
    foreach ($n in @('server-version', 'site-links', 'lan', 'public-host')) { Assert (CheckFailed $h1.result $n) "hil-run-check-$n-fails" }
    Assert ($h1.result.summary -ceq 'HIL v1 failure: server-version, site-links, lan, public-host (8 of 12 ok)') "hil-run-summary-names-checks-only ($($h1.result.summary))"
    $panic = @($h1.result.checks | Where-Object { $_.name -eq 'panic' })[0]
    Assert ($panic.numbers.spawns -eq 1 -and $panic.numbers.callbacks -gt 0) 'hil-run-panic-numbers-in-the-result'
    $card = @($h1.result.checks | Where-Object { $_.name -eq 'card' })[0]
    Assert ($card.numbers.frames -eq 32 -and $card.numbers.missed -eq 0 -and $card.numbers.resets -eq 0 -and $card.numbers.callbacks -ge 2850) 'hil-run-card-numbers-in-the-result'
    $ts = @($h1.result.checks | Where-Object { $_.name -eq 'test-signal' })[0]
    Assert ($ts.numbers.outputs -eq 2 -and $ts.numbers.after -eq 0 -and [math]::Abs($ts.numbers.lowest_dbfs + 30) -lt 0.01) "hil-run-test-signal-numbers-in-the-result ($($ts.detail))"

    # HIL's spare outputs that never reach the asked level, or still sound after the TTL
    # (the silence wait ends after -EngineWait), fail the test-signal check alone.
    $h11 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":0,"heard":0}') 'dev' '0.2' @('-EngineWait', '1')
    $d11 = CheckDetail $h11.result 'test-signal'
    Assert ((CheckFailed $h11.result 'test-signal') -and ($d11 -like '*peaked at -150.0 dBFS*')) "hil-run-a-signal-never-heard-fails ($d11)"
    $h12 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":0,"heard":1000}') 'dev' '0.2' @('-EngineWait', '1')
    $d12 = CheckDetail $h12.result 'test-signal'
    Assert ((CheckFailed $h12.result 'test-signal') -and ($d12 -like '*2 spare output(s) still sound after the TTL*')) "hil-run-a-signal-that-stays-fails ($d12)"
    foreach ($n in @('card', 'reopen', 'panic', 'alarm-push')) {
        foreach ($h in @($h11, $h12)) { Assert (CheckOk $h.result $n) "hil-run-a-failed-signal-leaves-$n ($(CheckDetail $h.result $n))" }
    }
    # A short TTL's signal may show only in a status that arrives after the TTL (the engine
    # sends Status about once a second, with the peaks since its previous one): every status
    # read before the silence that follows the signal counts, so the level on the third
    # status after test-signal, the first two silent, passes.
    $h13 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":0,"heard_from":3}')
    $d13 = CheckDetail $h13.result 'test-signal'
    Assert (CheckOk $h13.result 'test-signal') "hil-run-a-signal-heard-only-after-the-ttl-passes ($d13)"
    $ts13 = @($h13.result.checks | Where-Object { $_.name -eq 'test-signal' })[0]
    Assert ($ts13.numbers.outputs -eq 2 -and $ts13.numbers.after -eq 0 -and [math]::Abs($ts13.numbers.lowest_dbfs + 30) -lt 0.01) "hil-run-a-signal-heard-only-after-the-ttl-numbers ($d13)"

    $h2 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":["job-begin"],"silent":[],"event_after":0}')
    Assert ($h2.exit -eq 0 -and $h2.result.conclusion -ceq 'cancelled' -and $h2.result.summary -ceq 'HIL v1 cancelled: the PC was not free (job-begin refused)') "hil-run-a-refused-job-begin-is-cancelled ($($h2.result.summary))"
    Assert ($h2.result.why -like 'job-begin refused: fake job-begin*' -and $h2.result.summary -notlike '*fake*') 'hil-run-the-guards-detail-stays-in-why-never-in-the-public-summary'
    Assert ($h2.calls.Count -eq 1) "hil-run-a-refused-job-begin-touches-nothing-else ($($h2.calls -join ' | '))"

    $h3 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":3}')
    Assert ($h3.exit -eq 0 -and $h3.result.conclusion -ceq 'cancelled' -and $h3.result.summary -ceq 'HIL v1 cancelled: the guard left dev') "hil-run-a-switch-to-event-cancels (exit $($h3.exit), $($h3.result.summary))"
    Assert ($h3.result.why -like '*the guard left dev*' -and $h3.result.summary -notlike '*fake*') 'hil-run-a-cancelled-switch-keeps-its-text-in-why'
    Assert (-not (@($h3.calls) -like 'report *') -and (@($h3.calls) -contains 'job-end 4242') -and -not (@($h3.calls) -contains 'force-reopen')) "hil-run-a-cancelled-job-reports-nothing ($($h3.calls -join ' | '))"

    $h4 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":["job-begin"],"event_after":0}')
    Assert ($h4.exit -eq 1 -and $h4.result.conclusion -ceq 'failure' -and (CheckFailed $h4.result 'job-begin') -and $h4.calls.Count -eq 1) 'hil-run-an-unreachable-guard-fails'

    $h5 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":["report"],"silent":[],"event_after":0}')
    Assert ($h5.exit -eq 1 -and (CheckFailed $h5.result 'report')) 'hil-run-a-refused-report-is-a-failure'

    $h6 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":0}') 'feature'
    Assert ($h6.exit -eq 1 -and (CheckFailed $h6.result 'inputs') -and $h6.calls.Count -eq 0) 'hil-run-refuses-bad-inputs-before-any-call'

    $h8 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":0}') 'dev' '0'
    Assert ($h8.exit -eq 1 -and (CheckFailed $h8.result 'inputs') -and $h8.calls.Count -eq 0) 'hil-run-refuses-a-zero-ttl-before-any-call'

    $h7 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":["inject-fault"],"silent":[],"event_after":0}')
    Assert ($h7.exit -eq 1 -and (CheckFailed $h7.result 'panic') -and (CheckOk $h7.result 'alarm-push')) 'hil-run-a-refused-fault-injection-fails-the-panic-check'

    # The guard while the engine comes up (activate, the respawn after the fault): no
    # engine in its reply, then the engine's first Status (frames 0), and a forced
    # reopen's reset shows a few statuses late. HIL v1 waits for the engine it expects.
    $h9 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":0,"cold":2}')
    foreach ($n in @('activate', 'engine-build', 'card', 'pipes', 'test-signal', 'reopen', 'panic', 'alarm-push')) {
        Assert (CheckOk $h9.result $n) "hil-run-waits-for-the-engine-check-$n-passes ($(CheckDetail $h9.result $n))"
    }
    Assert ($h9.result.summary -ceq $h1.result.summary) "hil-run-waits-for-the-engine-same-summary ($($h9.result.summary))"

    # An engine that never shows: every wait ends after -EngineWait (-PanicWait for the
    # respawn) and its checks fail on what the guard showed.
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $h10 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":0,"cold":1000}') 'dev' '0.2' @('-EngineWait', '1', '-PanicWait', '1')
    $took = $clock.Elapsed.TotalSeconds
    $eb = CheckDetail $h10.result 'engine-build'
    Assert (($h10.exit -eq 1) -and (CheckFailed $h10.result 'engine-build') -and ($eb -ceq "engine build ''")) "hil-run-an-engine-that-never-shows-fails-engine-build ($eb)"
    foreach ($n in @('card', 'reopen', 'panic')) { Assert (CheckFailed $h10.result $n) "hil-run-an-engine-that-never-shows-fails-$n" }
    Assert ($took -lt 60) "hil-run-the-engine-waits-are-bounded ($took s)"
} finally {
    $sch = New-Object -ComObject 'Schedule.Service'
    $sch.Connect()
    foreach ($p in @($folder, '\iemmixer-test-none')) {
        try {
            $tf = $sch.GetFolder($p)
            foreach ($t in @($tf.GetTasks(1))) { $tf.DeleteTask($t.Name, 0) }
            $sch.GetFolder('\').DeleteFolder($p.TrimStart('\'), 0)
        } catch { Write-Host "cleanup: task folder $p ($($_.Exception.Message))" }
    }
    Remove-NetFirewallRule -Name $ruleName -ErrorAction SilentlyContinue
    if (Get-Service -Name $svcName -ErrorAction SilentlyContinue) { & sc.exe delete $svcName | Out-Null }
    Remove-Item -LiteralPath $regKey -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -Path 'Env:\ACTIONS_RUNNER_INPUT_TOKEN' -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $base -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host 'Test-IemPc: all passed'
