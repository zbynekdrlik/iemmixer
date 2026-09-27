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

    # ---- Register-IemTasks on the real Task Scheduler ----
    $prefArgs = @{ PrefKey = $regKey; PrefName = 'Pref'; PrefOriginal = '64' }
    $sch = New-Object -ComObject 'Schedule.Service'
    $sch.Connect()
    $e = ErrorOf { Register-IemTasks -Root $root -AppExe $appExe -Folder '\iemmixer-test-none' -ElevatedDir $elevated @prefArgs }
    Assert ($e -like '*iemmixer-StartREAPER is missing*') "tasks-need-the-existing-start-reaper-task ($e)"
    Throws { $sch.GetFolder('\iemmixer-test-none') } 'tasks-refused-before-any-folder-exists'
    Assert (-not (Test-Path -LiteralPath $elevated)) 'tasks-refused-before-the-elevated-folder'
    Throws { Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedDir $elevated -PrefKey $regKey -PrefName 'Pref' -PrefOriginal '64x' } 'tasks-refuse-a-non-numeric-original'
    Throws { Register-IemTasks -Root ($root + '"') -AppExe $appExe -Folder $folder -ElevatedDir $elevated @prefArgs } 'tasks-refuse-a-quote-in-a-path'

    Register-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'iemmixer-StartREAPER' `
        -Action (New-ScheduledTaskAction -Execute 'cmd.exe' -Argument '/c exit 0') `
        -Principal (New-ScheduledTaskPrincipal -UserId $me.name -LogonType Interactive -RunLevel Limited) | Out-Null
    $reports = Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedDir $elevated @prefArgs
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
    $entry = Join-Path $elevated 'iem-task.ps1'
    Assert ($l.Actions[0].Arguments -like "*-File `"$entry`" -Root `"$root`" -Kind logon -PrefKey `"$regKey`" -PrefName `"Pref`" -PrefOriginal `"64`"") "tasks-logon-runs-the-elevated-entry ($($l.Actions[0].Arguments))"
    $tu = Get-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'iemmixer-tuning'
    Assert ($tu.Actions[0].Arguments -like "*-File `"$entry`" -Root `"$root`" -Kind tuning" -and $tu.Actions[0].Execute -like '*\WindowsPowerShell\v1.0\powershell.exe') 'tasks-tuning-runs-the-elevated-entry'
    $sr = Get-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'iemmixer-StartREAPER'
    Assert ($sr.Actions[0].Execute -eq 'cmd.exe' -and $sr.Actions[0].Arguments -eq '/c exit 0') 'tasks-start-reaper-keeps-its-action'
    foreach ($n in @('iemmixer-StartREAPER', 'iemmixer-guard', 'iemmixer-exclude')) {
        $sd = $sch.GetFolder($folder).GetTask($n).GetSecurityDescriptor(4)
        Assert (Test-IemTaskSddl -Sddl $sd -UserSid $me.sid) "tasks-$n-descriptor-lets-the-user-run-it ($sd)"
    }
    # The elevated folder: this module and the entry, changeable only by Administrators and SYSTEM.
    Assert ((Get-FileHash -LiteralPath (Join-Path $elevated 'IemPc.psm1')).Hash -eq (Get-FileHash -LiteralPath (Join-Path $here 'IemPc.psm1')).Hash) 'elevated-folder-holds-this-module'
    $tokens = $null; $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($entry, [ref]$tokens, [ref]$errors)
    Assert ($errors.Count -eq 0) 'elevated-entry-parses'
    $ea = Get-Acl -LiteralPath $elevated
    $er = @($ea.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
    $userRule = @($er | Where-Object { $_.IdentityReference.Value -eq $me.sid })
    Assert ($ea.AreAccessRulesProtected -and $er.Count -eq 3 -and $userRule.Count -eq 1 -and [int]$userRule[0].FileSystemRights -eq 1179817) 'elevated-folder-protected-user-reads-only'
    Assert ((Sorted ($er | ForEach-Object { $_.IdentityReference.Value })) -eq (Sorted @($me.sid, 'S-1-5-18', 'S-1-5-32-544'))) 'elevated-folder-user-system-administrators'
    $again = Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedDir $elevated @prefArgs
    Assert ($again.Count -eq 7) 'tasks-register-again-idempotent'

    # ---- the root's DACL ----
    New-Item -ItemType Directory -Force -Path (Join-Path $root 'bundles') | Out-Null
    Set-Content -LiteralPath (Join-Path $root 'bundles\x.txt') -Value 'x'
    $acl = Get-Acl -LiteralPath $root
    $acl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule('Everyone', 'Read', 'ContainerInherit, ObjectInherit', 'None', 'Allow')))
    Set-Acl -LiteralPath $root -AclObject $acl
    $childRules = @((Get-Acl -LiteralPath (Join-Path $root 'bundles\x.txt')).GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
    Assert (@($childRules | Where-Object { $_.IdentityReference.Value -eq 'S-1-1-0' }).Count -eq 1) 'root-acl-precondition-everyone-reaches-a-child'
    $a1 = Set-IemRootAcl -Root $root
    Assert $a1.changed 'root-acl-changes-an-open-root'
    $ra = Get-Acl -LiteralPath $root
    $rules = @($ra.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
    Assert ($ra.AreAccessRulesProtected -and $rules.Count -eq 3) 'root-acl-protected-with-three-rules'
    Assert ((Sorted ($rules | ForEach-Object { $_.IdentityReference.Value })) -eq (Sorted @($me.sid, 'S-1-5-18', 'S-1-5-32-544'))) 'root-acl-user-system-administrators'
    Assert (@($rules | Where-Object { [int]$_.FileSystemRights -ne 2032127 -or [int]$_.InheritanceFlags -ne 3 -or $_.IsInherited }).Count -eq 0) 'root-acl-full-control-inherited-below'
    $childRules = @((Get-Acl -LiteralPath (Join-Path $root 'bundles\x.txt')).GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
    Assert ($childRules.Count -eq 3 -and @($childRules | Where-Object { -not $_.IsInherited -or $_.IdentityReference.Value -eq 'S-1-1-0' }).Count -eq 0) 'root-acl-reaches-existing-children'
    $a2 = Set-IemRootAcl -Root $root
    Assert (-not $a2.changed) 'root-acl-second-run-changes-nothing'

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
    $lg = Invoke-IemTaskRequest -Kind logon -Root $root -TuningDir $noTuning -PrefKey $regKey -PrefName 'Pref' -PrefOriginal '64'
    $pv = Get-IemPref -Key $regKey -Name 'Pref'
    Assert ($lg.ok -and $pv.value -eq 64 -and $pv.kind -eq 'DWord' -and $lg.result.pref.attempts -eq 1 -and $lg.result.tuning -eq 'absent') 'logon-restores-the-preference-keeping-its-kind'
    Assert ((Get-Content -LiteralPath (Join-Path $root 'guard\tasks\logon.result.json') -Raw | ConvertFrom-Json).ok) 'logon-writes-its-result-file'
    $r0 = Restore-IemPref -Key $regKey -Name 'Pref' -Original '64'
    Assert ($r0.ok -and $r0.attempts -eq 0) 'pref-at-its-original-is-not-written'
    New-ItemProperty -LiteralPath $regKey -Name 'Text' -Value '32' -PropertyType String | Out-Null
    $rt = Restore-IemPref -Key $regKey -Name 'Text' -Original '064'
    $tv = Get-IemPref -Key $regKey -Name 'Text'
    Assert ($rt.ok -and $tv.raw -ceq '064' -and $tv.kind -eq 'String') 'pref-restores-the-raw-text-of-a-string'
    Throws { Restore-IemPref -Key $regKey -Name 'Pref' -Original '6 4' } 'pref-refuses-a-non-numeric-original'
    Assert ((ConvertTo-IemHkcuPath -Key 'Software\ASIO\Test Card') -ceq 'HKCU:\Software\ASIO\Test Card') 'pref-site-key-is-under-hkcu'
    $nk = Invoke-IemTaskRequest -Kind logon -Root $root -TuningDir $noTuning -PrefKey $regKey -PrefName 'NoSuchValue' -PrefOriginal '64'
    Assert (-not $nk.ok -and $nk.error) 'logon-reports-a-missing-value'

    $td = Join-Path $root 'guard\tasks'
    [IO.File]::WriteAllText((Join-Path $td 'tuning.request.json'), '{"id":"t-1","verb":"state"}')
    $tr = Invoke-IemTaskRequest -Kind tuning -Root $root -TuningDir $noTuning
    Assert ($tr.ok -and $tr.id -ceq 't-1' -and $tr.result -eq 'absent') 'tuning-request-is-absent-before-s1c'
    [IO.File]::WriteAllText((Join-Path $td 'tuning.request.json'), '{"id":"t-2","verb":"format"}')
    $tr = Invoke-IemTaskRequest -Kind tuning -Root $root -TuningDir $noTuning
    Assert (-not $tr.ok -and $tr.id -ceq 't-2' -and $tr.error -like '*refused*') 'tuning-request-refuses-an-unknown-verb'
    [IO.File]::WriteAllText((Join-Path $td 'tuning.request.json'), '{"id":"t 3; x","verb":"state"}')
    Assert (-not (Invoke-IemTaskRequest -Kind tuning -Root $root -TuningDir $noTuning).ok) 'task-request-refuses-a-bad-id'
    [IO.File]::WriteAllText((Join-Path $td 'exclude.request.json'), '{"id":"x-1","sha":"..\\..\\x","keep":[]}')
    $xr = Invoke-IemTaskRequest -Kind exclude -Root $root
    Assert (-not $xr.ok -and $xr.error -like '*not a bundle SHA*') 'exclude-request-refuses-a-bad-sha'
    Throws { Invoke-IemTaskRequest -Kind format -Root $root } 'task-request-refuses-an-unknown-kind'
    # The generated entry, as the task runs it.
    [IO.File]::WriteAllText((Join-Path $td 'tuning.request.json'), '{"id":"t-4","verb":"exit"}')
    & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $entry -Kind tuning -Root $root | Out-Null
    $code = $LASTEXITCODE
    $res = Get-Content -LiteralPath (Join-Path $td 'tuning.result.json') -Raw | ConvertFrom-Json
    Assert ($code -eq 0 -and $res.ok -and $res.id -ceq 't-4') "elevated-entry-runs-a-request (exit $code)"

    # ---- bootstrap state (read only) ----
    $bs = Get-IemBootstrapState -Root $root -Module 'iemmixer-no-such-module.dll' -PrefKey $regKey -PrefName 'Pref' -AppImage 'iemmixer-no-such-app.exe' -Folder $folder -FirewallRule $ruleName
    Assert ($bs.holders.Count -eq 0 -and $bs.app -eq 0 -and $bs.reaper -eq 0 -and $bs.pref.value -eq 64) 'bootstrap-state-holders-processes-and-preference'
    Assert (@($bs.tasks | Where-Object { -not $_.exists -or -not $_.sddl_ok }).Count -eq 0 -and @($bs.tasks).Count -eq 7) 'bootstrap-state-our-tasks-with-their-descriptors'
    Assert ($bs.root.acl_ok -and $bs.firewall.present -and -not $bs.firewall.ok) 'bootstrap-state-root-and-the-disabled-test-rule'
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
$n = @(Get-Content -LiteralPath $log).Count
if (@($sc.silent) -contains $cmd) { exit 4 }
$reopens = @(Get-Content -LiteralPath $log | Where-Object { $_ -eq 'force-reopen' }).Count
$mode = 'dev'
if ($sc.event_after -gt 0 -and $n -gt $sc.event_after) { $mode = 'event' }
$ok = -not (@($sc.refuse) -contains $cmd)
$reply = [ordered]@{ ok = $ok; mode = $mode; switching = $null; alarms = @(); detail = ('fake ' + $cmd) }
if ($cmd -eq 'status') {
    $reply['engine'] = [ordered]@{ build = $sc.sha; frames = 32; callbacks = (3000 * $n); missed = 0; resets = $reopens
                                   parked = $false; faulted = $false; pipe_private = $true }
}
Write-Output (ConvertTo-Json -InputObject $reply -Depth 5 -Compress)
if ($ok) { exit 0 }
exit 1
'@
    [IO.File]::WriteAllText($fake, $fakeText)
    function Invoke-HilRun([string]$scenario, [string]$branch = 'dev') {
        [IO.File]::WriteAllText((Join-Path $hb 'scenario.json'), $scenario)
        foreach ($f in @('calls.log', 'result.json')) { Remove-Item -LiteralPath (Join-Path $hb $f) -ErrorAction SilentlyContinue }
        & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File (Join-Path $hdir 'hil-v1.ps1') -Sha $S -Branch $branch `
            -JobRun 4242 -Out (Join-Path $hb 'result.json') -Iemmode $fake -Local 'http://127.0.0.1:9' -CardSeconds 1 -TestTtl 0.2 | Out-Null
        $code = $LASTEXITCODE
        $calls = @()
        if (Test-Path -LiteralPath (Join-Path $hb 'calls.log')) { $calls = @(Get-Content -LiteralPath (Join-Path $hb 'calls.log')) }
        $res = Get-Content -LiteralPath (Join-Path $hb 'result.json') -Raw | ConvertFrom-Json
        return [pscustomobject]@{ exit = $code; result = $res; calls = $calls }
    }
    function CheckOk($res, [string]$name) { return @($res.checks | Where-Object { $_.name -eq $name -and $_.ok }).Count -eq 1 }
    function CheckFailed($res, [string]$name) { return @($res.checks | Where-Object { $_.name -eq $name -and -not $_.ok }).Count -eq 1 }

    $h1 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":0}')
    $calls = $h1.calls
    Assert ($h1.exit -eq 1 -and $h1.result.conclusion -ceq 'failure' -and $h1.result.sha -ceq $S -and $h1.result.job_run -ceq '4242') "hil-run-with-the-server-down-fails (exit $($h1.exit))"
    Assert ($calls[0] -ceq 'job-begin 4242' -and $calls[1] -ceq "activate $S") "hil-run-begins-the-job-then-activates ($($calls -join ' | '))"
    Assert ($calls -contains 'test-signal mic1 -30 0.2' -and $calls -contains 'force-reopen' -and $calls -contains 'alarm-test') 'hil-run-drives-the-signal-reopen-and-alarm'
    Assert ($calls[$calls.Count - 2] -ceq 'job-end 4242' -and $calls[$calls.Count - 1] -like "report $S red HIL v1 failure: *") 'hil-run-ends-the-job-then-reports-red'
    foreach ($n in @('activate', 'engine-build', 'card', 'pipes', 'test-signal', 'reopen', 'alarm-push')) { Assert (CheckOk $h1.result $n) "hil-run-check-$n-passes" }
    foreach ($n in @('server-version', 'site-links', 'lan', 'public-host', 'panic')) { Assert (CheckFailed $h1.result $n) "hil-run-check-$n-fails" }
    Assert ($h1.result.summary -ceq 'HIL v1 failure: server-version, site-links, lan, public-host, panic (7 of 12 ok)') "hil-run-summary-names-checks-only ($($h1.result.summary))"
    $card = @($h1.result.checks | Where-Object { $_.name -eq 'card' })[0]
    Assert ($card.numbers.frames -eq 32 -and $card.numbers.missed -eq 0 -and $card.numbers.resets -eq 0 -and $card.numbers.callbacks -ge 2850) 'hil-run-card-numbers-in-the-result'

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
