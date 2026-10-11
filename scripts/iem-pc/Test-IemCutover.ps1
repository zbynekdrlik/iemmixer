#Requires -Version 5.1
# Self-test of IemCutover.psm1 (S8 lane 2, #11: the predecessor's autostarts
# exported, disabled and re-enabled, the guard task's logon trigger) on
# Windows PowerShell 5.1 (CI job windows, an ephemeral administrator runner).
# The module runs as the stage holds it: next to IemPc.psm1 (with its
# siblings, #36) and S1c's IemTuningStore.psm1, copied into one folder. Everything runs against TEST
# objects: our tasks in \iemmixer-test-cut-<id>\ (registered by
# Register-IemTasks, as on the PC), the "predecessor's" tasks in
# \iemmixer-test-cut-<id>-pred\, its Run values under
# HKCU:\Software\iemmixer-cut-test-<id>, its Startup-folder shortcuts (S8 lane
# 6) in a test folder inside the runner's profile, passed as -StartupDir (the
# self-test's override; the task's entry never passes it), a temp root and
# elevated root: never \iemmixer, never a real Run key, never the real
# Startup folder. The task's body runs through
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
# IemPc.psm1's siblings (#36), which it imports from its own folder: the stage holds them too
# (by their names; test_iempc_bundle checks that these files are exactly its $script:SiblingNames).
$pcSiblings = @(Get-ChildItem -LiteralPath $here -File | Where-Object { $_.Name -cmatch '^IemPc[A-Za-z0-9]+\.psm1$' } | ForEach-Object { $_.FullName })
foreach ($f in $pcSiblings) { $sources[(Split-Path -Leaf $f)] = $f }
$pcSiblingNames = @($pcSiblings | ForEach-Object { Split-Path -Leaf $_ })
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
# The predecessor's Startup-folder shortcuts (invented names and bytes) in a
# folder inside the profile (the module refuses one outside it; the temp
# folder may be named by its 8.3 short name, so it is not used here).
$local = [Environment]::GetFolderPath('LocalApplicationData')
$startup = Join-Path $local ('iem-cut-startup-' + $id)
$junction = Join-Path $local ('iem-cut-junction-' + $id)
$links = @('Zyxtray App.lnk', 'zyxother.lnk')
$linkBytes = @{ 'Zyxtray App.lnk' = [byte[]](0x4C, 0, 0, 0, 1, 20, 2, 0, 0xFF, 0x7A); 'zyxother.lnk' = [byte[]](0x4C, 0, 0, 0, 9, 8, 7) }
$install = @{ ModuleSha256 = $sums; Root = $root; Tasks = $tasks; RunValues = $runValues; Startup = $links; Folder = $folder; ElevatedRoot = $er
              StartupDir = $startup }
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

function Write-Link([string]$Name, [byte[]]$Bytes) { [IO.File]::WriteAllBytes((Join-Path $startup $Name), $Bytes) }

function Read-Links {
    # Each listed shortcut as "same" (the bytes the predecessor's has), "other", or "absent".
    $out = @()
    foreach ($n in $links) {
        $p = Join-Path $startup $n
        if (-not (Test-Path -LiteralPath $p)) { $out += 'absent'; continue }
        if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($p)) -ceq [Convert]::ToBase64String($linkBytes[$n])) { $out += 'same' } else { $out += 'other' }
    }
    return ($out -join ' ; ')
}

function Send-Request([string]$Verb, $Export) {
    # The guard's request file, then the task's body (with the test's Startup folder).
    $dir = Join-Path $root 'guard\tasks'
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $rid = 'test-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
    $doc = [pscustomobject]@{ id = $rid; verb = $Verb; export = $Export }
    [IO.File]::WriteAllText((Join-Path $dir 'cutover.request.json'), (ConvertTo-Json -InputObject $doc -Compress), (New-Object System.Text.UTF8Encoding $false))
    $r = Invoke-IemCutoverRequest -Root $root -ElevatedRoot $er -Folder $folder -StartupDir $startup
    $file = [IO.File]::ReadAllText((Join-Path $er 'tasks\out\cutover.result.json')) | ConvertFrom-Json
    Assert ($file.id -ceq $rid -and $file.kind -ceq 'cutover' -and $file.ok -eq $r.ok) "result-file-answers-request-$Verb"
    return $r
}

$apps = 'String:C:\Tools\app.exe --tray'
$trays = 'ExpandString:%IEMTESTVAR%\tray.exe'
$both = "$apps ; $trays"

New-Item -ItemType Directory -Force -Path $root | Out-Null
New-Item -ItemType Directory -Force -Path $startup | Out-Null
foreach ($n in $links) { Write-Link $n $linkBytes[$n] }
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
    # S8 lane 6: a shortcut is a bare name.lnk, enough alone, never twice.
    Assert ((Test-IemAutostartList -Startup $links).Count -eq 0) 'list-takes-shortcuts-alone'
    Assert ((Test-IemAutostartList -Startup @('Zyx (x64).LNK')).Count -eq 0) 'list-takes-a-shortcut-with-an-upper-case-extension'
    foreach ($s in @('sub\zyx.lnk', 'sub/zyx.lnk', '..\zyx.lnk', 'zyx..lnk', 'C:zyx.lnk', 'zyx.lnk:alt', 'zyx.txt', '.lnk', 'zyx".lnk',
                     '%zyx%.lnk', 'zyx!.lnk', 'zyx^.lnk', 'zyx&.lnk', 'zyx|.lnk', 'zyx<.lnk', 'zyx*.lnk', 'zyx?.lnk', ("zyx`t.lnk"),
                     ("zyx.lnk`n"), ('zy' + [char]0xE9 + '.lnk'))) {
        $bad = Test-IemAutostartList -Startup @($s)
        Assert ($bad.Count -gt 0 -and ($bad -join ' | ') -like "*shortcut '*' refused*") "list-refuses-the-shortcut [$s] ($($bad -join ' | '))"
    }
    # The whole value, as iempc's fullmatch: .NET's $ would also match before a final line break.
    Assert ((Test-IemAutostartList -Tasks @("\a\b`n")).Count -gt 0 -and (Test-IemAutostartList -RunValues @("HKCU:\x|app`n")).Count -gt 0) 'list-refuses-a-final-line-break'
    $bad = Test-IemAutostartList -Startup @('zyx.lnk', 'ZYX.LNK')
    Assert (($bad -join ' | ') -like '*named twice*') "list-refuses-a-shortcut-named-twice ($($bad -join ' | '))"
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
    # S8 lane 6: each listed shortcut must exist in the Startup folder.
    Remove-Item -LiteralPath (Join-Path $startup 'zyxother.lnk')
    $e = ErrorOf { Install-IemCutover @install }
    Assert ($e -like '*the shortcut zyxother.lnk does not exist in the Startup folder*' -and -not (Test-Path -LiteralPath $cutDir)) "install-refuses-a-missing-shortcut ($e)"
    Write-Link 'zyxother.lnk' $linkBytes['zyxother.lnk']
    Assert ((Read-Links) -ceq 'same ; same') "shortcuts-as-the-predecessor-left-them ($(Read-Links))"
    # The Startup folder must lie inside the user's profile (ProfileList), with
    # no junction or link from the profile down to it or at the shortcut.
    $outside = $install.Clone()
    $outside.StartupDir = Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) ('iem-cut-none-' + $id)
    $e = ErrorOf { Install-IemCutover @outside }
    Assert ($e -like '*is not inside the profile*' -and -not (Test-Path -LiteralPath $cutDir)) "install-refuses-a-startup-folder-outside-the-profile ($e)"
    $relative = $install.Clone()
    $relative.StartupDir = 'iem-cut-relative'
    $e = ErrorOf { Install-IemCutover @relative }
    Assert ($e -like "*Startup folder is 'iem-cut-relative': refused*" -and -not (Test-Path -LiteralPath $cutDir)) "install-refuses-a-relative-startup-folder ($e)"
    New-Item -ItemType Directory -Force -Path (Join-Path $startup 'sub') | Out-Null
    New-Item -ItemType Junction -Path $junction -Target $startup | Out-Null
    foreach ($via in @($junction, (Join-Path $junction 'sub'))) {
        $linked = $install.Clone()
        $linked.StartupDir = $via
        $e = ErrorOf { Install-IemCutover @linked }
        Assert ($e -like "*$junction is a junction or a link: the Startup folder is refused*" -and -not (Test-Path -LiteralPath $cutDir)) "install-refuses-a-startup-folder-through-a-junction [$via] ($e)"
    }
    [IO.Directory]::Delete($junction)
    $aside = Join-Path $base 'zyxother-aside.lnk'
    Move-Item -LiteralPath (Join-Path $startup 'zyxother.lnk') -Destination $aside
    New-Item -ItemType SymbolicLink -Path (Join-Path $startup 'zyxother.lnk') -Target $aside | Out-Null
    $e = ErrorOf { Install-IemCutover @install }
    Assert ($e -like '*zyxother.lnk is a junction or a link: refused*' -and -not (Test-Path -LiteralPath $cutDir)) "install-refuses-a-shortcut-that-is-a-link ($e)"
    [IO.File]::Delete((Join-Path $startup 'zyxother.lnk'))
    # A folder by a listed shortcut's name, an empty shortcut and one past the size cap.
    New-Item -ItemType Directory -Path (Join-Path $startup 'zyxother.lnk') | Out-Null
    $e = ErrorOf { Install-IemCutover @install }
    Assert ($e -like '*zyxother.lnk is no file: refused*' -and -not (Test-Path -LiteralPath $cutDir)) "install-refuses-a-folder-by-a-shortcut-s-name ($e)"
    Remove-Item -LiteralPath (Join-Path $startup 'zyxother.lnk')
    Write-Link 'zyxother.lnk' ([byte[]]@())
    $e = ErrorOf { Install-IemCutover @install }
    Assert ($e -like '*the shortcut zyxother.lnk is empty*' -and -not (Test-Path -LiteralPath $cutDir)) "install-refuses-an-empty-shortcut ($e)"
    Write-Link 'zyxother.lnk' (New-Object byte[] 1048577)
    $e = ErrorOf { Install-IemCutover @install }
    Assert ($e -like '*zyxother.lnk is larger than 1048576 bytes: refused*' -and -not (Test-Path -LiteralPath $cutDir)) "install-refuses-a-shortcut-past-the-cap ($e)"
    [IO.File]::Delete((Join-Path $startup 'zyxother.lnk'))
    Move-Item -LiteralPath $aside -Destination (Join-Path $startup 'zyxother.lnk')
    Assert ((Read-Links) -ceq 'same ; same') "shortcuts-back-after-the-link ($(Read-Links))"
    $missingDir = $install.Clone()
    $missingDir.StartupDir = Join-Path $local ('iem-cut-missing-' + $id)
    $e = ErrorOf { Install-IemCutover @missingDir }
    Assert ($e -like '*the Startup folder * does not exist: refused*' -and -not (Test-Path -LiteralPath $cutDir)) "install-refuses-a-startup-folder-that-does-not-exist ($e)"
    # Without -StartupDir the folder is this session's own (the task's way), inside the profile.
    $cut = Get-Module IemCutover
    $own = [Environment]::GetFolderPath('Startup')
    $e = ErrorOf { $script:resolved = & $cut { param($sid) Resolve-IemStartupDir -UserSid $sid } $me.sid }
    if ($own) {
        Assert ($e -eq '' -and $script:resolved -ceq [IO.Path]::GetFullPath($own).TrimEnd('\')) "resolve-takes-this-session-s-startup-folder ($e)"
    } else {
        Assert ($e -like "*this session's Startup folder is '': refused*") "resolve-refuses-a-session-without-a-startup-folder ($e)"
    }
    # The profile as ProfileList records it: literal, or from %SystemDrive%; nothing else.
    $drive = [IO.Path]::GetPathRoot([Environment]::SystemDirectory).TrimEnd('\')
    Assert ((& $cut { ConvertFrom-IemProfilePath -Raw '%SystemDrive%\Users\zyxuser' }) -ceq ($drive + '\Users\zyxuser')) 'profile-from-the-system-drive'
    Assert ((& $cut { ConvertFrom-IemProfilePath -Raw 'D:\Users\zyxuser\' }) -ceq 'D:\Users\zyxuser') 'profile-taken-literally'
    foreach ($p in @('%USERPROFILE%', 'Users\zyxuser', '\\host\share\zyxuser', '', '%SystemDrive%\%x%')) {
        $e = ErrorOf { & $cut { param($x) ConvertFrom-IemProfilePath -Raw $x } $p }
        Assert ($e -like '*a profile recorded as * is refused*') "profile-refuses [$p] ($e)"
    }
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
    # A sibling of IemPc.psm1 (#36) is checked as the module is: other bytes, or no sha256 for it.
    $wrong.ModuleSha256 = $sums.Clone()
    $wrong.ModuleSha256[$pcSiblingNames[0]] = '0' * 64
    $e = ErrorOf { Install-IemCutover @wrong }
    Assert ($e -like "*$($pcSiblingNames[0])*is not the build iempc checked*" -and -not (Test-Path -LiteralPath $cutDir)) "install-refuses-a-sibling-that-is-not-the-checked-build ($e)"
    $wrong.ModuleSha256 = $sums.Clone()
    $wrong.ModuleSha256.Remove($pcSiblingNames[-1])
    $e = ErrorOf { Install-IemCutover @wrong }
    Assert ($e -like "*no sha256 for $($pcSiblingNames[-1])*" -and -not (Test-Path -LiteralPath $cutDir)) "install-refuses-a-sibling-without-its-sha256 ($e)"
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
    foreach ($n in @(@('IemCutover.psm1', 'IemPc.psm1', 'IemTuningStore.psm1', 'iem-cutover.ps1', 'autostarts.json') + $pcSiblingNames)) {
        $p = Join-Path $cutDir $n
        Assert ((Test-IemElevatedItem -Path $p -UserSid $me.sid).Count -eq 0) "install-writes-$n-admin-only"
    }
    foreach ($n in @(@('IemCutover.psm1', 'IemPc.psm1', 'IemTuningStore.psm1') + $pcSiblingNames)) {
        Assert ((Get-FileHash -LiteralPath (Join-Path $cutDir $n) -Algorithm SHA256).Hash.ToLowerInvariant() -ceq $sums[$n]) "install-copies-$n-as-checked"
    }
    $list = [IO.File]::ReadAllText((Join-Path $cutDir 'autostarts.json')) | ConvertFrom-Json
    Assert ((@($list.tasks) -join '|') -ceq ($tasks -join '|') -and (@($list.run) -join '|') -ceq ($runValues -join '|') -and $list.version -eq 1) 'install-writes-the-list'
    Assert ((@($list.startup) -join '|') -ceq ($links -join '|') -and (@($r.startup) -join '|') -ceq ($links -join '|')) 'install-writes-the-shortcuts-into-the-list'
    Assert (-not ([IO.File]::ReadAllText((Join-Path $cutDir 'autostarts.json'))).Contains((Split-Path -Leaf $startup))) 'the-list-names-no-startup-folder'
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
    # earlier cutover never undone) refuses the export, nothing written.
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
    # S8 lane 6: a listed shortcut already absent (an earlier cutover never undone) too.
    Remove-Item -LiteralPath (Join-Path $startup 'Zyxtray App.lnk')
    $r = Send-Request 'autostarts-off' $export
    Assert (-not $r.ok -and $r.error -like '*the shortcut Zyxtray App.lnk is absent from the Startup folder*' -and -not (Test-Path -LiteralPath (Join-Path $cutDir $export)) -and
            (Read-Run) -ceq $both -and [bool](Get-PredTask 'appstart').Enabled -and (Read-Links) -ceq 'absent ; same') "disable-refuses-a-shortcut-already-absent ($($r.error))"
    Write-Link 'Zyxtray App.lnk' $linkBytes['Zyxtray App.lnk']

    # ---- disable: exported, then disabled, each read back ----
    $r = Send-Request 'autostarts-off' $export
    Assert ($r.ok -and $r.result.state -ceq 'disabled' -and $r.result.tasks -eq 2 -and $r.result.values -eq 2 -and $r.result.shortcuts -eq 2) "disable-answers ($(ConvertTo-Json -InputObject $r -Compress -Depth 6))"
    Assert (-not [bool](Get-PredTask 'appstart').Enabled -and -not [bool](Get-PredTask 'other').Enabled) 'disable-disables-every-task'
    Assert ((Read-Run) -ceq 'absent ; absent') "disable-removes-every-run-value ($(Read-Run))"
    Assert ((Read-Links) -ceq 'absent ; absent') "disable-removes-every-shortcut ($(Read-Links))"
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
    # S8 lane 6: each shortcut's bytes as startup-<n>.lnk, named with its sha256.
    Assert ((@($ex.startup | ForEach-Object { [string]$_.name }) -join '|') -ceq ($links -join '|')) 'export-names-each-shortcut'
    $k = 0
    foreach ($s in @($ex.startup)) {
        $k++
        $f = Join-Path $exDir ([string]$s.file)
        Assert ([string]$s.file -ceq ('startup-{0}.lnk' -f $k)) "export-file-of-$($s.name)-is-startup-$k"
        Assert ([Convert]::ToBase64String([IO.File]::ReadAllBytes($f)) -ceq [Convert]::ToBase64String($linkBytes[[string]$s.name])) "export-saves-$($s.name)-byte-for-byte"
        Assert ((Get-FileHash -LiteralPath $f -Algorithm SHA256).Hash.ToLowerInvariant() -ceq [string]$s.sha256) "export-names-the-sha256-of-$($s.name)"
        Assert ((Test-IemElevatedItem -Path $f -UserSid $me.sid).Count -eq 0) "export-copy-of-$($s.name)-is-admin-only"
    }
    Assert (-not ([IO.File]::ReadAllText((Join-Path $exDir 'export.json'))).Contains((Split-Path -Leaf $startup))) 'the-export-names-no-startup-folder'
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
    Assert ((Read-Links) -ceq 'same ; same' -and (@($r.result.shortcuts | ForEach-Object { $_.state }) -join ',') -ceq 'written,written') "enable-writes-every-shortcut-back-exactly ($(Read-Links))"
    $r = Send-Request 'autostarts-on' $export
    Assert ($r.ok -and $r.result.state -ceq 'enabled' -and (Read-Run) -ceq $both -and (Read-Links) -ceq 'same ; same' -and
            (@($r.result.shortcuts | ForEach-Object { $_.state }) -join ',') -ceq 'kept,kept') 'enable-again-changes-nothing'
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

    # ---- S8 lane 6: a shortcut with other bytes is never overwritten ----
    $r = Send-Request 'autostarts-off' 'autostarts-6'
    Assert ($r.ok -and (Read-Links) -ceq 'absent ; absent' -and (Read-Run) -ceq 'absent ; absent') 'disable-into-a-shortcut-export'
    $ex6 = Join-Path $cutDir 'autostarts-6'
    $marker6 = Join-Path $ex6 'restored.json'
    # A changed copy in the export is never written back: nothing is restored.
    $copy = Join-Path $ex6 'startup-1.lnk'
    $orig = [IO.File]::ReadAllBytes($copy)
    [IO.File]::WriteAllBytes($copy, [byte[]]($orig + [byte[]](0x00)))
    $r = Send-Request 'autostarts-on' 'autostarts-6'
    Assert (-not $r.ok -and $r.error -like '*startup-1.lnk is not the file the export saved*' -and (Read-Links) -ceq 'absent ; absent' -and
            (Read-Run) -ceq 'absent ; absent' -and -not [bool](Get-PredTask 'appstart').Enabled) "enable-refuses-a-shortcut-copy-that-changed ($($r.error))"
    [IO.File]::WriteAllBytes($copy, $orig)
    # A shortcut there with other bytes is refused, never overwritten; the
    # tasks, the values and the other shortcut come back, the export stays unrestored.
    Write-Link 'zyxother.lnk' ([byte[]](0x4C, 0, 0, 0, 1))
    $r = Send-Request 'autostarts-on' 'autostarts-6'
    Assert (-not $r.ok -and $r.error -like '*the tasks and the Run values are back, not every shortcut*' -and
            $r.error -like '*the shortcut zyxother.lnk is in the Startup folder with other bytes than the export saved: refused, never overwritten*' -and
            (Read-Links) -ceq 'same ; other' -and (Read-Run) -ceq $both -and [bool](Get-PredTask 'appstart').Enabled -and [bool](Get-PredTask 'other').Enabled -and
            -not (Test-Path -LiteralPath $marker6)) "enable-refuses-a-shortcut-with-other-bytes-and-restores-the-rest ($($r.error))"
    $r = Send-Request 'autostarts-off' 'autostarts-8'
    Assert (-not $r.ok -and $r.error -like '*the export autostarts-6 of an earlier cutover was never restored*' -and
            -not (Test-Path -LiteralPath (Join-Path $cutDir 'autostarts-8'))) "an-export-left-unrestored-refuses-the-next ($($r.error))"
    # Put right by hand: the one with the saved bytes is kept, the other written.
    Remove-Item -LiteralPath (Join-Path $startup 'zyxother.lnk')
    $r = Send-Request 'autostarts-on' 'autostarts-6'
    Assert ($r.ok -and (Read-Links) -ceq 'same ; same' -and (Read-Run) -ceq $both -and (Test-Path -LiteralPath $marker6 -PathType Leaf) -and
            (@($r.result.shortcuts | ForEach-Object { $_.state }) -join ',') -ceq 'kept,written') "enable-keeps-a-shortcut-with-the-saved-bytes ($(ConvertTo-Json -InputObject $r.result.shortcuts -Compress))"

    # ---- S8 lane 6: a removal that fails puts back what the disable changed ----
    $ro = Join-Path $startup 'zyxother.lnk'
    [IO.File]::SetAttributes($ro, [IO.FileAttributes]::ReadOnly)
    $r = Send-Request 'autostarts-off' 'autostarts-7'
    [IO.File]::SetAttributes($ro, [IO.FileAttributes]::Normal)
    Assert (-not $r.ok -and $r.error -like '*what was disabled is enabled again*' -and (Read-Links) -ceq 'same ; same' -and (Read-Run) -ceq $both -and
            [bool](Get-PredTask 'appstart').Enabled -and [bool](Get-PredTask 'other').Enabled -and
            (Test-Path -LiteralPath (Join-Path $cutDir 'autostarts-7\restored.json') -PathType Leaf)) "disable-that-fails-at-a-shortcut-puts-everything-back ($($r.error))"

    # ---- a task that is gone: nothing re-enabled, its saved XML named ----
    $r = Send-Request 'autostarts-off' 'autostarts-3'
    Assert ($r.ok) 'disable-into-a-third-export'
    $pf = (Connect-IemScheduler).GetFolder($pred)
    $pf.DeleteTask('other', 0)
    $r = Send-Request 'autostarts-on' 'autostarts-3'
    Assert (-not $r.ok -and $r.error -like "*the task $pred\other no longer exists: nothing re-enabled*" -and (Read-Run) -ceq 'absent ; absent' -and
            (Read-Links) -ceq 'absent ; absent' -and -not [bool](Get-PredTask 'appstart').Enabled) "enable-refuses-while-a-task-is-gone ($($r.error))"

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
    # The junction and the link first, as links (never what they point to), then the test Startup folder.
    try { if (Test-IemReparsePoint -Path $junction) { [IO.Directory]::Delete($junction) } } catch { Write-Host "cleanup: the junction ($($_.Exception.Message))" }
    $sym = Join-Path $startup 'zyxother.lnk'
    try { if (Test-IemReparsePoint -Path $sym) { [IO.File]::Delete($sym) } } catch { Write-Host "cleanup: the link ($($_.Exception.Message))" }
    Remove-Item -LiteralPath $startup -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $base -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host 'Test-IemCutover: all passed'
