#Requires -Version 5.1
# Self-test of IemPc.psm1's other bootstrap steps on Windows PowerShell 5.1 (CI
# job windows, an ephemeral administrator runner; split out of Test-IemPc.ps1,
# #36): the service right on a test service, the bundle sums and Defender's
# exclusions (-WhatIf), the HIL runner (-WhatIf; no ops token), the tunnel's
# origin and the predecessor's facts (read only, a test task folder
# \iemmixer-test\). Only its own test objects are removed.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
Import-Module (Join-Path $here 'IemPc.psm1') -Force
function Assert($cond, $what) { if (-not $cond) { throw "FAILED: $what" } ; Write-Host "ok  $what" }
function Throws([scriptblock]$b, $what) { $t = $false; try { & $b } catch { $t = $true }; Assert $t $what }
function ErrorOf([scriptblock]$b) { try { & $b; return '' } catch { return "$_" } }
function Sorted($items) { return ((@($items) | ForEach-Object { "$_" } | Sort-Object) -join ',') }

$id = [guid]::NewGuid().ToString('N').Substring(0, 8)
$base = Join-Path ([IO.Path]::GetTempPath()) ('iempc-boot-test-' + $id)
$root = Join-Path $base 'root'
$folder = '\iemmixer-test'
$svcName = 'iemmixer-test-' + $id
$appExe = Join-Path $env:SystemRoot 'System32\cmd.exe'
# A synthetic user (the service grant's SDDL), never 'LA' (Test-IemPc.ps1).
$userSid = 'S-1-5-21-1000000001-1000000002-1000000003-1001'
New-Item -ItemType Directory -Force -Path $root | Out-Null

try {
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
} finally {
    $sch = New-Object -ComObject 'Schedule.Service'
    $sch.Connect()
    try {
        $tf = $sch.GetFolder($folder)
        foreach ($t in @($tf.GetTasks(1))) { $tf.DeleteTask($t.Name, 0) }
        $sch.GetFolder('\').DeleteFolder($folder.TrimStart('\'), 0)
    } catch { Write-Host "cleanup: task folder $folder ($($_.Exception.Message))" }
    if (Get-Service -Name $svcName -ErrorAction SilentlyContinue) { & sc.exe delete $svcName | Out-Null }
    Remove-Item -Path 'Env:\ACTIONS_RUNNER_INPUT_TOKEN' -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $base -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host 'Test-IemPcBootstrap: all passed'
