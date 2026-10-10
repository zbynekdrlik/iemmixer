#Requires -Version 5.1
# Self-test of IemCutover.psm1 (S8 lane 2, #11: the predecessor's autostarts
# exported, disabled and re-enabled, the guard task's logon trigger) on
# Windows PowerShell 5.1 (CI job windows, an ephemeral administrator runner).
# The module runs as the stage holds it: next to IemPc.psm1 and S1c's
# IemTuningStore.psm1, copied into one folder. Everything runs against TEST
# objects: our tasks in \iemmixer-test-cut-<id>\ (registered by
# Register-IemTasks, as on the PC), the "predecessor's" tasks in
# \iemmixer-test-cut-<id>-pred\, its Run values under
# HKCU:\Software\iemmixer-cut-test-<id>, a temp root and elevated root: never
# \iemmixer, never a real Run key. The task's body runs through
# Invoke-IemCutoverRequest with a request file, as Test-IemPc.ps1 runs
# Invoke-IemTaskRequest. Only its own test objects are removed.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
function Assert($cond, $what) { if (-not $cond) { throw "FAILED: $what" } ; Write-Host "ok  $what" }
function ErrorOf([scriptblock]$b) { try { & $b; return '' } catch { return "$_" } }

$id = [guid]::NewGuid().ToString('N').Substring(0, 8)
$base = Join-Path ([IO.Path]::GetTempPath()) ('iem-cutover-' + $id)
$mods = Join-Path $base 'modules'
$sources = @{ 'IemPc.psm1' = (Join-Path $here 'IemPc.psm1'); 'IemCutover.psm1' = (Join-Path $here 'IemCutover.psm1')
              'IemTuningStore.psm1' = (Join-Path (Split-Path -Parent $here) 'pc-tuning\IemTuningStore.psm1') }
New-Item -ItemType Directory -Force -Path $mods | Out-Null
foreach ($n in @($sources.Keys)) { Copy-Item -LiteralPath $sources[$n] -Destination (Join-Path $mods $n) }
Import-Module (Join-Path $mods 'IemCutover.psm1') -Force

$root = Join-Path $base 'root'
$er = Join-Path $base 'elevated'
$folder = '\iemmixer-test-cut-' + $id
$pred = '\iemmixer-test-cut-' + $id + '-pred'
$runKey = 'HKCU:\Software\iemmixer-cut-test-' + $id + '\Run'
$prefKey = 'HKCU:\Software\iemmixer-cut-test-' + $id + '\Pref'
$me = Resolve-IemUser
$appExe = Join-Path $env:SystemRoot 'System32\cmd.exe'
$sums = @{}
foreach ($n in @($sources.Keys)) { $sums[$n] = (Get-FileHash -LiteralPath (Join-Path $mods $n) -Algorithm SHA256).Hash.ToLowerInvariant() }
$tasks = @(($pred + '\appstart'), ($pred + '\other'))
$runValues = @(($runKey + '|app'), ($runKey + '|tray'))
$install = @{ ModuleSha256 = $sums; Root = $root; Tasks = $tasks; RunValues = $runValues; Folder = $folder; ElevatedRoot = $er }
$cutDir = Join-Path $er 'cutover'
$export = 'autostarts-1790000000'

function Get-PredTask([string]$Name) {
    $sch = Connect-IemScheduler
    return (Get-IemRegisteredTask -Scheduler $sch -Folder $pred -Name $Name)
}

function Get-GuardTriggers {
    $sch = Connect-IemScheduler
    $t = Get-IemRegisteredTask -Scheduler $sch -Folder $folder -Name 'iemmixer-guard'
    return (@((Get-IemTaskReport -Task $t -UserSid $me.sid).triggers) -join ',')
}

function Read-Run {
    # Each Run value as "<kind>:<data>" (unexpanded), or "absent".
    $out = @()
    foreach ($n in @('app', 'tray')) {
        $r = Get-IemRegRaw -Path $runKey -Name $n
        if ($r.kind -ceq 'absent') { $out += 'absent'; continue }
        $out += ('{0}:{1}' -f $r.kind, $r.data)
    }
    return ($out -join ' ; ')
}

function Send-Request([string]$Verb, $Export) {
    # The guard's request file, then the task's body.
    $dir = Join-Path $root 'guard\tasks'
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $rid = 'test-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
    $doc = [pscustomobject]@{ id = $rid; verb = $Verb; export = $Export }
    [IO.File]::WriteAllText((Join-Path $dir 'cutover.request.json'), (ConvertTo-Json -InputObject $doc -Compress), (New-Object System.Text.UTF8Encoding $false))
    $r = Invoke-IemCutoverRequest -Root $root -ElevatedRoot $er -Folder $folder
    $file = [IO.File]::ReadAllText((Join-Path $er 'tasks\out\cutover.result.json')) | ConvertFrom-Json
    Assert ($file.id -ceq $rid -and $file.kind -ceq 'cutover' -and $file.ok -eq $r.ok) "result-file-answers-request-$Verb"
    return $r
}

$apps = 'String:C:\Tools\app.exe --tray'
$trays = 'ExpandString:%IEMTESTVAR%\tray.exe'
$both = "$apps ; $trays"

New-Item -ItemType Directory -Force -Path $root | Out-Null
New-Item -Path $prefKey -Force | Out-Null
New-ItemProperty -LiteralPath $prefKey -Name 'Pref' -Value 64 -PropertyType DWord | Out-Null
try {
    # ---- the list's rules (pure) ----
    Assert ((Test-IemAutostartList -Tasks $tasks -RunValues $runValues).Count -eq 0) 'list-takes-task-paths-and-run-values'
    Assert ((Test-IemAutostartList -Tasks @('\a') -RunValues @()).Count -eq 0) 'list-takes-a-task-at-the-root'
    $cases = @(
        @(@(), @(), 'no autostart is named'),
        @(@('a\b'), @(), "task path 'a\b' refused"),
        @(@('\a\'), @(), "task path '\a\' refused"),
        @(@('\a"b'), @(), 'refused'),
        @(@('\a%x%'), @(), 'refused'),
        @(@(), @('HKCU:\Software\Run'), "Run value 'HKCU:\Software\Run' refused"),
        @(@(), @('HKU:\x\Run|app'), 'refused'),
        @(@(), @('HKCU:\Software\Run\|app'), 'refused'),
        @(@(), @('HKCU:\Software\Run|a|b'), 'refused'),
        @(@('\a\b', '\A\B'), @(), 'named twice'),
        @(@(), @('HKCU:\x|app', 'hkcu:\X|APP'), 'named twice'))
    foreach ($c in $cases) {
        $bad = Test-IemAutostartList -Tasks $c[0] -RunValues $c[1]
        Assert ($bad.Count -gt 0 -and ($bad -join ' | ') -like ('*' + $c[2] + '*')) "list-refuses [$(@($c[0]) -join ',')] [$(@($c[1]) -join ',')] ($($bad -join ' | '))"
    }
    # The task XML's comparison (pure): exact, but for the Settings' Enabled element, or not.
    $x1 = '<?xml version="1.0" encoding="UTF-16"?><Task xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task"><Settings><Enabled>true</Enabled><Priority>7</Priority></Settings><Triggers><LogonTrigger><Enabled>true</Enabled></LogonTrigger></Triggers></Task>'
    $x2 = '<?xml version="1.0" encoding="UTF-16"?><Task xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task"><Settings><Priority>7</Priority></Settings><Triggers><LogonTrigger><Enabled>true</Enabled></LogonTrigger></Triggers></Task>'
    $x3 = $x2.Replace('<LogonTrigger><Enabled>true', '<LogonTrigger><Enabled>false')
    Assert ((Compare-IemTaskXml -Saved $x1 -Now $x1) -ceq 'exact') 'xml-the-same-is-exact'
    Assert ((Compare-IemTaskXml -Saved $x1 -Now $x2) -ceq 'enabled-element') 'xml-but-the-settings-enabled-element'
    Assert ((Compare-IemTaskXml -Saved $x1 -Now $x3) -ceq 'differs') 'xml-a-trigger-s-enabled-differs'
    Assert ((Compare-IemTaskXml -Saved 'not xml' -Now '<a/>') -ceq 'differs') 'xml-that-does-not-parse-differs'

    # ---- our tasks, as Register-IemTasks makes them on the PC ----
    Register-ScheduledTask -TaskPath ($folder + '\') -TaskName 'iemmixer-StartREAPER' `
        -Action (New-ScheduledTaskAction -Execute 'cmd.exe' -Argument '/c exit 0') `
        -Principal (New-ScheduledTaskPrincipal -UserId $me.name -LogonType Interactive -RunLevel Limited) | Out-Null
    $prefArgs = @{ PrefKey = $prefKey; PrefName = 'Pref'; PrefOriginal = '64'; Module = 'testcard.dll' }
    [void](Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedRoot $er @prefArgs)
    Assert ((Get-GuardTriggers) -ceq '') 'guard-task-starts-with-no-trigger-before-the-cutover'

    # ---- the predecessor: two tasks and two Run values ----
    foreach ($n in @('appstart', 'other')) {
        Register-ScheduledTask -TaskPath ($pred + '\') -TaskName $n `
            -Action (New-ScheduledTaskAction -Execute 'cmd.exe' -Argument '/c exit 0') `
            -Trigger (New-ScheduledTaskTrigger -AtLogOn -User $me.name) `
            -Principal (New-ScheduledTaskPrincipal -UserId $me.name -LogonType Interactive -RunLevel Limited) | Out-Null
    }
    $xmlBefore = @{}
    foreach ($n in @('appstart', 'other')) { $xmlBefore[$n] = [string](Get-PredTask $n).Xml }

    # ---- the install's refusals: nothing changes ----
    $missing = $install.Clone()
    $missing.Tasks = @($tasks + @($pred + '\none'))
    $e = ErrorOf { Install-IemCutover @missing }
    Assert ($e -like "*the task $pred\none does not exist*" -and -not (Test-Path -LiteralPath $cutDir)) "install-refuses-a-task-that-does-not-exist ($e)"
    $e = ErrorOf { Install-IemCutover @install }
    Assert ($e -like "*the value $runKey|app does not exist*" -and -not (Test-Path -LiteralPath $cutDir)) "install-refuses-a-run-value-that-does-not-exist ($e)"
    New-Item -Path $runKey -Force | Out-Null
    New-ItemProperty -LiteralPath $runKey -Name 'app' -Value 'C:\Tools\app.exe --tray' -PropertyType String | Out-Null
    $e = ErrorOf { Install-IemCutover @install }
    Assert ($e -like "*the value $runKey|tray does not exist*" -and -not (Test-Path -LiteralPath $cutDir)) "install-refuses-each-missing-run-value ($e)"
    New-ItemProperty -LiteralPath $runKey -Name 'tray' -Value '%IEMTESTVAR%\tray.exe' -PropertyType ExpandString | Out-Null
    Assert ((Read-Run) -ceq $both) "run-values-as-the-predecessor-left-them ($(Read-Run))"
    # S8 lane 5: a listed task already disabled is an earlier cutover's state.
    (Get-PredTask 'other').Enabled = $false
    $e = ErrorOf { Install-IemCutover @install }
    Assert ($e -like "*the task $pred\other is already disabled*" -and -not (Test-Path -LiteralPath $cutDir)) "install-refuses-a-task-already-disabled ($e)"
    (Get-PredTask 'other').Enabled = $true
    $wrong = $install.Clone()
    $wrong.ModuleSha256 = $sums.Clone()
    $wrong.ModuleSha256['IemCutover.psm1'] = '0' * 64
    $e = ErrorOf { Install-IemCutover @wrong }
    Assert ($e -like '*is not the build iempc checked*' -and -not (Test-Path -LiteralPath $cutDir)) "install-refuses-a-module-that-is-not-the-checked-build ($e)"
    $noFolder = $install.Clone()
    $noFolder.Folder = $folder + '-none'
    $e = ErrorOf { Install-IemCutover @noFolder }
    Assert ($e -like '*does not exist (Register-IemTasks makes it)*' -and -not (Test-Path -LiteralPath $cutDir)) "install-refuses-a-missing-task-folder ($e)"
    $inside = $install.Clone()
    $inside.Root = Join-Path $er 'user'
    $e = ErrorOf { Install-IemCutover @inside }
    Assert ($e -like '*must not contain each other*') "install-refuses-a-root-inside-the-elevated-root ($e)"

    # ---- the install ----
    $r = Install-IemCutover @install
    Assert ($r.state -ceq 'installed' -and $r.task -ceq ($folder + '\iemmixer-cutover')) "install-registers-the-cutover-task ($($r.task))"
    foreach ($n in @('IemCutover.psm1', 'IemPc.psm1', 'IemTuningStore.psm1', 'iem-cutover.ps1', 'autostarts.json')) {
        $p = Join-Path $cutDir $n
        Assert ((Test-IemElevatedItem -Path $p -UserSid $me.sid).Count -eq 0) "install-writes-$n-admin-only"
    }
    foreach ($n in @('IemCutover.psm1', 'IemPc.psm1', 'IemTuningStore.psm1')) {
        Assert ((Get-FileHash -LiteralPath (Join-Path $cutDir $n) -Algorithm SHA256).Hash.ToLowerInvariant() -ceq $sums[$n]) "install-copies-$n-as-checked"
    }
    $list = [IO.File]::ReadAllText((Join-Path $cutDir 'autostarts.json')) | ConvertFrom-Json
    Assert ((@($list.tasks) -join '|') -ceq ($tasks -join '|') -and (@($list.run) -join '|') -ceq ($runValues -join '|') -and $list.version -eq 1) 'install-writes-the-list'
    $sch = Connect-IemScheduler
    $ct = Get-IemRegisteredTask -Scheduler $sch -Folder $folder -Name 'iemmixer-cutover'
    $rep = Get-IemTaskReport -Task $ct -UserSid $me.sid
    Assert ($rep.run_level -eq 1 -and $rep.sddl_ok -and (Test-IemTaskReport -Report $rep -RunLevel 1).Count -eq 0 -and @($rep.triggers).Count -eq 0) 'cutover-task-is-highest-runnable-by-the-user-no-trigger'
    Assert (([string]@($rep.actions)[0].arguments) -like ('*-File "' + (Join-Path $cutDir 'iem-cutover.ps1') + '" -Root "' + $root + '" -Folder "' + $folder + '"')) "cutover-task-runs-the-entry-with-the-root-and-folder ($(@($rep.actions)[0].arguments))"
    $again = Install-IemCutover @install
    Assert ($again.state -ceq 'installed') 'install-runs-again'

    # ---- the task's refusals ----
    $r = Send-Request 'reboot' $export
    Assert (-not $r.ok -and $r.error -like "*cutover verb 'reboot' refused*") "task-refuses-another-verb ($($r.error))"
    $r = Send-Request 'autostarts-off' '..\x'
    Assert (-not $r.ok -and $r.error -like "*export name '..\x' refused*" -and (Read-Run) -ceq $both) "task-refuses-another-export-name ($($r.error))"
    # S8 lane 5: a task already disabled or a Run value already absent (an
    # earlier cutover never rolled back) refuses the export, nothing written.
    (Get-PredTask 'other').Enabled = $false
    $r = Send-Request 'autostarts-off' $export
    Assert (-not $r.ok -and $r.error -like "*the task $pred\other is already disabled*" -and -not (Test-Path -LiteralPath (Join-Path $cutDir $export)) -and
            (Read-Run) -ceq $both -and [bool](Get-PredTask 'appstart').Enabled) "disable-refuses-a-task-already-disabled ($($r.error))"
    (Get-PredTask 'other').Enabled = $true
    Remove-ItemProperty -LiteralPath $runKey -Name 'tray'
    $r = Send-Request 'autostarts-off' $export
    Assert (-not $r.ok -and $r.error -like "*the value $runKey|tray is absent*" -and -not (Test-Path -LiteralPath (Join-Path $cutDir $export)) -and
            [bool](Get-PredTask 'appstart').Enabled -and [bool](Get-PredTask 'other').Enabled) "disable-refuses-a-run-value-already-absent ($($r.error))"
    New-ItemProperty -LiteralPath $runKey -Name 'tray' -Value '%IEMTESTVAR%\tray.exe' -PropertyType ExpandString | Out-Null
    Assert ((Read-Run) -ceq $both) "run-values-back-after-the-refusals ($(Read-Run))"

    # ---- disable: exported, then disabled, each read back ----
    $r = Send-Request 'autostarts-off' $export
    Assert ($r.ok -and $r.result.state -ceq 'disabled' -and $r.result.tasks -eq 2 -and $r.result.values -eq 2) "disable-answers ($(ConvertTo-Json -InputObject $r -Compress -Depth 6))"
    Assert (-not [bool](Get-PredTask 'appstart').Enabled -and -not [bool](Get-PredTask 'other').Enabled) 'disable-disables-every-task'
    Assert ((Read-Run) -ceq 'absent ; absent') "disable-removes-every-run-value ($(Read-Run))"
    $exDir = Join-Path $cutDir $export
    $ex = [IO.File]::ReadAllText((Join-Path $exDir 'export.json')) | ConvertFrom-Json
    Assert ($ex.version -eq 1 -and $ex.export -ceq $export) 'export-names-itself'
    $byPath = @{}
    foreach ($t in @($ex.tasks)) { $byPath[[string]$t.path] = $t }
    Assert ([bool]$byPath[$pred + '\appstart'].enabled -and [bool]$byPath[$pred + '\other'].enabled) 'export-saves-each-task-s-enabled-state'
    foreach ($n in @('appstart', 'other')) {
        $t = $byPath[$pred + '\' + $n]
        $f = Join-Path $exDir ([string]$t.xml)
        $bytes = [IO.File]::ReadAllBytes($f)
        Assert ($bytes[0] -eq 0xFF -and $bytes[1] -eq 0xFE) "export-xml-of-$n-is-utf16-with-its-bom"
        Assert ([IO.File]::ReadAllText($f, [Text.Encoding]::Unicode) -ceq $xmlBefore[$n]) "export-xml-of-$n-is-the-task-s-xml-byte-for-byte"
        Assert ((Get-FileHash -LiteralPath $f -Algorithm SHA256).Hash.ToLowerInvariant() -ceq [string]$t.sha256) "export-names-the-sha256-of-$n"
        Assert ((Test-IemElevatedItem -Path $f -UserSid $me.sid).Count -eq 0) "export-xml-of-$n-is-admin-only"
    }
    $byName = @{}
    foreach ($v in @($ex.run)) { $byName[[string]$v.name] = $v }
    Assert ($byName['app'].raw.kind -ceq 'String' -and $byName['app'].raw.data -ceq 'C:\Tools\app.exe --tray') 'export-saves-a-string-value-exactly'
    Assert ($byName['tray'].raw.kind -ceq 'ExpandString' -and $byName['tray'].raw.data -ceq '%IEMTESTVAR%\tray.exe') 'export-saves-an-expandable-value-unexpanded'
    Assert ((Test-IemElevatedItem -Path $exDir -UserSid $me.sid).Count -eq 0 -and (Test-IemElevatedItem -Path (Join-Path $exDir 'export.json') -UserSid $me.sid).Count -eq 0) 'export-is-admin-only'
    $r = Send-Request 'autostarts-off' $export
    Assert (-not $r.ok -and $r.error -like '*an export is never overwritten*') "disable-never-overwrites-an-export ($($r.error))"
    $r = Send-Request 'autostarts-off' 'autostarts-5'
    Assert (-not $r.ok -and $r.error -like "*the export $export of an earlier cutover was never restored*" -and
            -not (Test-Path -LiteralPath (Join-Path $cutDir 'autostarts-5'))) "disable-refuses-while-an-earlier-export-is-not-restored ($($r.error))"

    # ---- enable: back exactly as saved, then nothing more ----
    $r = Send-Request 'autostarts-on' $export
    Assert ($r.ok -and $r.result.state -ceq 'enabled' -and $r.result.values -eq 2) "enable-answers ($(ConvertTo-Json -InputObject $r -Compress -Depth 6))"
    Assert ([bool](Get-PredTask 'appstart').Enabled -and [bool](Get-PredTask 'other').Enabled) 'enable-restores-each-task-s-saved-state'
    $marker = Join-Path $exDir 'restored.json'
    Assert ((Test-Path -LiteralPath $marker -PathType Leaf) -and (Test-IemElevatedItem -Path $marker -UserSid $me.sid).Count -eq 0) 'enable-marks-the-export-restored-admin-only'
    foreach ($s in @($r.result.tasks)) {
        Assert (@('exact', 'enabled-element') -ccontains [string]$s.xml) "enable-reads-back-the-xml-of $($s.task) ($($s.xml))"
        Write-Host "     $($s.task): $($s.xml)"
    }
    Assert ((Read-Run) -ceq $both) "enable-writes-every-value-back-exactly ($(Read-Run))"
    $r = Send-Request 'autostarts-on' $export
    Assert ($r.ok -and $r.result.state -ceq 'enabled' -and (Read-Run) -ceq $both) 'enable-again-changes-nothing'
    $r = Send-Request 'autostarts-on' 'autostarts-1'
    Assert ($r.ok -and $r.result.state -ceq 'none') 'enable-of-an-export-that-does-not-exist-does-nothing'

    # ---- a tampered export is never restored from ----
    $r = Send-Request 'autostarts-off' 'autostarts-2'
    Assert ($r.ok -and (Read-Run) -ceq 'absent ; absent') 'disable-into-a-second-export'
    $ex2 = Join-Path $cutDir 'autostarts-2'
    $xmlFile = Join-Path $ex2 'task-1.xml'
    $orig = [IO.File]::ReadAllBytes($xmlFile)
    [IO.File]::WriteAllBytes($xmlFile, [byte[]]($orig + [byte[]](0x20, 0x00)))
    $r = Send-Request 'autostarts-on' 'autostarts-2'
    Assert (-not $r.ok -and $r.error -like '*is not the file the export saved*' -and (Read-Run) -ceq 'absent ; absent') "enable-refuses-an-xml-file-that-changed ($($r.error))"
    [IO.File]::WriteAllBytes($xmlFile, $orig)
    $r = Send-Request 'autostarts-on' 'autostarts-2'
    Assert ($r.ok -and (Read-Run) -ceq $both -and [bool](Get-PredTask 'appstart').Enabled) 'enable-from-the-restored-file'
    # An export without its restore mark counts as never restored, whatever
    # the autostarts are now; enabling from it again marks it.
    Remove-Item -LiteralPath (Join-Path $ex2 'restored.json')
    $r = Send-Request 'autostarts-off' 'autostarts-4'
    Assert (-not $r.ok -and $r.error -like '*the export autostarts-2 of an earlier cutover was never restored*' -and
            -not (Test-Path -LiteralPath (Join-Path $cutDir 'autostarts-4')) -and (Read-Run) -ceq $both) "disable-refuses-an-export-without-its-restore-mark ($($r.error))"
    $r = Send-Request 'autostarts-on' 'autostarts-2'
    Assert ($r.ok -and (Test-Path -LiteralPath (Join-Path $ex2 'restored.json') -PathType Leaf)) 'enable-again-marks-the-export-restored'

    # ---- a task that is gone: nothing re-enabled, its saved XML named ----
    $r = Send-Request 'autostarts-off' 'autostarts-3'
    Assert ($r.ok) 'disable-into-a-third-export'
    $pf = (Connect-IemScheduler).GetFolder($pred)
    $pf.DeleteTask('other', 0)
    $r = Send-Request 'autostarts-on' 'autostarts-3'
    Assert (-not $r.ok -and $r.error -like "*the task $pred\other no longer exists: nothing re-enabled*" -and (Read-Run) -ceq 'absent ; absent' -and
            -not [bool](Get-PredTask 'appstart').Enabled) "enable-refuses-while-a-task-is-gone ($($r.error))"

    # ---- the guard task's logon trigger, kept by Register-IemTasks ----
    $r = Send-Request 'logon-on' $null
    Assert ($r.ok -and $r.result.state -ceq 'set' -and (Get-GuardTriggers) -ceq '9') "logon-on-gives-the-guard-task-its-logon-trigger ($(Get-GuardTriggers))"
    $g = Get-IemTaskReport -Task (Get-IemRegisteredTask -Scheduler (Connect-IemScheduler) -Folder $folder -Name 'iemmixer-guard') -UserSid $me.sid
    Assert ((Test-IemTaskReport -Report $g -RunLevel 0).Count -eq 0 -and $g.sddl_ok) 'logon-on-keeps-the-guard-task-as-register-made-it'
    $r = Send-Request 'logon-on' $null
    Assert ($r.ok -and $r.result.state -ceq 'unchanged') 'logon-on-again-changes-nothing'
    $reports = Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedRoot $er @prefArgs
    $guardRow = @($reports | Where-Object { $_.task -ceq 'iemmixer-guard' })[0]
    Assert ((Get-GuardTriggers) -ceq '9' -and @($guardRow.problems).Count -eq 0) 'register-again-keeps-the-guard-s-logon-trigger'
    $r = Send-Request 'logon-off' $null
    Assert ($r.ok -and $r.result.state -ceq 'set' -and (Get-GuardTriggers) -ceq '') "logon-off-removes-it ($(Get-GuardTriggers))"
    [void](Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedRoot $er @prefArgs)
    Assert ((Get-GuardTriggers) -ceq '') 'register-again-adds-no-trigger-before-the-cutover'
} finally {
    $sch = New-Object -ComObject 'Schedule.Service'
    $sch.Connect()
    foreach ($tf in @($folder, $pred)) {
        try {
            $f = $sch.GetFolder($tf)
            foreach ($t in @($f.GetTasks(1))) { $f.DeleteTask($t.Name, 0) }
            $sch.GetFolder('\').DeleteFolder($tf.TrimStart('\'), 0)
        } catch { Write-Host "cleanup: task folder $tf ($($_.Exception.Message))" }
    }
    try { Remove-Item -LiteralPath ('HKCU:\Software\iemmixer-cut-test-' + $id) -Recurse -Force } catch { Write-Host "cleanup: the test key ($($_.Exception.Message))" }
    Remove-Item -LiteralPath $base -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host 'Test-IemCutover: all passed'
