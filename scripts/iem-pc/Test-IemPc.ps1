#Requires -Version 5.1
# Self-test of the S6 PC module (IemPc.psm1 and its siblings, #36) on Windows
# PowerShell 5.1 (CI job windows, an ephemeral administrator runner), against
# real backends: a test task folder \iemmixer-test\ (security descriptors read
# back), a temp root, an HKCU test key and a disabled test firewall rule. Only
# its own test objects are removed. Split by responsibility (#36), each its
# own step of the windows job: Test-IemPcBootstrap.ps1 (the service right, the
# bundle sums and Defender, the runner, the tunnel and the predecessor's
# facts) and Test-IemPcHilV1.ps1 (HIL v1's helpers).
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
$regKey = 'HKCU:\Software\iemmixer-iempc-test-' + $id
$appExe = Join-Path $env:SystemRoot 'System32\cmd.exe'
$me = Resolve-IemUser
New-Item -ItemType Directory -Force -Path $root | Out-Null
New-Item -Path $regKey -Force | Out-Null
# The preference as the driver keeps it (a DWORD); Register-IemTasks reads its kind (#35).
New-ItemProperty -LiteralPath $regKey -Name 'Pref' -Value 64 -PropertyType DWord | Out-Null

try {
    # ---- the split (#36): one module every caller imports, its siblings from its own folder ----
    $pcFiles = @(Get-IemPcModuleFiles)
    $pcNames = @($pcFiles | ForEach-Object { Split-Path -Leaf $_ })
    Assert ($pcFiles.Count -ge 2 -and $pcFiles[-1] -eq (Join-Path $here 'IemPc.psm1') -and
            @($pcFiles | Where-Object { (Split-Path -Parent $_) -ne $here }).Count -eq 0) "pc-module-files-the-siblings-then-the-module ($($pcNames -join ', '))"
    $pcModules = @($pcNames | ForEach-Object { [IO.Path]::GetFileNameWithoutExtension($_) })
    foreach ($i in 0..($pcFiles.Count - 2)) {
        $m = @(Get-Module -Name $pcModules[$i])
        Assert ($m.Count -eq 1 -and $m[0].Path -eq $pcFiles[$i]) "pc-sibling-loaded-from-this-folder [$($pcModules[$i])]"
    }
    # Each function in exactly one of them, and every one the module had before the split (93) plus Get-IemPcModuleFiles.
    $pcFunctions = @(Get-Command -CommandType Function | Where-Object { $pcModules -contains $_.ModuleName })
    $twice = @($pcFunctions | Group-Object -Property Name | Where-Object { $_.Count -gt 1 } | ForEach-Object { $_.Name })
    Assert ($pcFunctions.Count -eq 94 -and $twice.Count -eq 0) "pc-modules-hold-every-function-once ($($pcFunctions.Count); twice: $($twice -join ', '))"
    # A value more than one of them reads is the same in each (the moved bodies read their own module's).
    foreach ($v in @('SidAdmins', 'SidSystem', 'ModulePattern', 'BootPrefLog')) {
        $vals = @{}
        foreach ($n in $pcModules) {
            $x = & (Get-Module -Name $n) { param($name) Get-Variable -Scope Script -Name $name -ValueOnly -ErrorAction SilentlyContinue } $v
            if ($null -ne $x) { $vals[$n] = "$x" }
        }
        Assert ($vals.Count -ge 2 -and @($vals.Values | Sort-Object -Unique).Count -eq 1) "pc-constant-the-same-in-each-module [$v] ($(@($vals.Keys) -join ','))"
    }

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

    # ---- the boot task's command line (pure, #35): reg.exe through cmd.exe, never PowerShell ----
    $sys32 = 'C:\Windows\System32'
    $logDir = 'C:\ProgramData\iemmixer\tasks\out'
    $bootArgs = @{ System = $sys32; PrefName = 'PrefBuffSize'; PrefOriginal = '64'; Module = 'testcard.dll'; LogDir = $logDir }
    # The value is read first and written only when it is not the original
    # (a normal boot writes nothing) and no process holds the driver module:
    # tasklist's CSV rows start with a quote (its "no tasks" line is localized
    # text, never read), which findstr /b looks for (\^" : the caret keeps
    # cmd's quote state, the backslash is findstr's own escape of the quote).
    # A quote line found: add=held, exit 0. Else the add runs only on proof
    # that the list was read: findstr /v finds a line that does not start
    # with a quote (tasklist's "no tasks" line, in any language); findstr
    # exits 1 both for "no match" and for a file it cannot open, so without
    # that proof nothing is written (add=unread, exit 2), as for a list
    # tasklist or its redirection could not write. A failed add is exit 1.
    $wantBoot = '/d /q /e:on /v:on /s /c ""C:\Windows\System32\reg.exe" query "HKCU\Software\ASIO\Test Card" /v "PrefBuffSize" >"C:\ProgramData\iemmixer\tasks\out\boot-pref.before" 2>&1 & set "iemadd=none" & "C:\Windows\System32\findstr.exe" /i /l /x /c:"    PrefBuffSize    REG_DWORD    0x40" "C:\ProgramData\iemmixer\tasks\out\boot-pref.before" >nul || ("C:\Windows\System32\tasklist.exe" /m "testcard.dll" /fo csv /nh >"C:\ProgramData\iemmixer\tasks\out\boot-pref.holders" 2>&1 && ("C:\Windows\System32\findstr.exe" /b /l \^" "C:\ProgramData\iemmixer\tasks\out\boot-pref.holders" >nul && set "iemadd=held" || ("C:\Windows\System32\findstr.exe" /v /b /l \^" "C:\ProgramData\iemmixer\tasks\out\boot-pref.holders" >nul && ("C:\Windows\System32\reg.exe" add "HKCU\Software\ASIO\Test Card" /v "PrefBuffSize" /t REG_DWORD /d 64 /f >nul 2>&1 && set "iemadd=0" || set "iemadd=1") || set "iemadd=unread")) || set "iemadd=unread") & (echo boot-pref !DATE! !TIME! add=!iemadd!)>>"C:\ProgramData\iemmixer\tasks\out\boot-pref.log" & "C:\Windows\System32\reg.exe" query "HKCU\Software\ASIO\Test Card" /v "PrefBuffSize" >>"C:\ProgramData\iemmixer\tasks\out\boot-pref.log" 2>&1 & if "!iemadd!"=="1" (exit /b 1) else if "!iemadd!"=="unread" (exit /b 2) else (exit /b 0)"'
    $bc = Get-IemBootPrefCommand -PrefKey 'Software\ASIO\Test Card' @bootArgs
    Assert ($bc.exe -ceq 'C:\Windows\System32\cmd.exe' -and $bc.arguments -ceq $wantBoot) "boot-pref-command-is-reg-exe-through-cmd ($($bc.arguments))"
    # The task runs elevated with the user's environment: no % (cmd and Task
    # Scheduler would expand a variable the user set there before cmd parses).
    Assert (-not $bc.arguments.Contains('%') -and [Environment]::ExpandEnvironmentVariables($bc.arguments) -ceq $bc.arguments) 'boot-pref-command-holds-no-percent-expansion'
    foreach ($k in @('HKCU:\Software\ASIO\Test Card', 'hkcu:Software\ASIO\Test Card\', 'Registry::HKEY_CURRENT_USER\Software\ASIO\Test Card', '\Software\ASIO\Test Card')) {
        $bk = Get-IemBootPrefCommand -PrefKey $k @bootArgs
        Assert ($bk.arguments -ceq $wantBoot) "boot-pref-command-names-the-key-under-hkcu [$k]"
    }
    $bt = Get-IemBootPrefCommand -PrefKey 'Software\ASIO\Test Card' -System $sys32 -PrefName 'PrefBuffSize' -PrefOriginal '064' -PrefKind 'text' -Module 'testcard.dll' -LogDir $logDir
    Assert ($bt.arguments -clike '* /c:"    PrefBuffSize    REG_SZ    064" * add "HKCU\Software\ASIO\Test Card" /v "PrefBuffSize" /t REG_SZ /d 064 /f >nul 2>&1 *') "boot-pref-command-keeps-a-string-kind ($($bt.arguments))"
    $b4 = Get-IemBootPrefCommand -PrefKey 'Software\ASIO\Test Card' -System $sys32 -PrefName 'PrefBuffSize' -PrefOriginal '1024' -Module 'testcard.dll' -LogDir $logDir
    Assert ($b4.arguments -clike '* /c:"    PrefBuffSize    REG_DWORD    0x400" * /t REG_DWORD /d 1024 /f *') "boot-pref-command-compares-the-dword-in-reg-exe-s-hex ($($b4.arguments))"
    $bx = Get-IemBootPrefCommand -PrefKey 'Software\ASIO\Test Card (x64)' @bootArgs
    Assert ($bx.arguments -clike '* add "HKCU\Software\ASIO\Test Card (x64)" /v "PrefBuffSize" *') 'boot-pref-command-takes-a-key-with-parentheses-in-quotes'
    # The holder check names the module the logon task reads (Register-IemTasks' -Module).
    $bm = Get-IemBootPrefCommand -PrefKey 'Software\ASIO\Test Card' -System $sys32 -PrefName 'PrefBuffSize' -PrefOriginal '64' -Module 'Test Card x64.dll' -LogDir $logDir
    Assert ($bm.arguments -clike '* ("C:\Windows\System32\tasklist.exe" /m "Test Card x64.dll" /fo csv /nh >"C:\ProgramData\iemmixer\tasks\out\boot-pref.holders" 2>&1 && (*') "boot-pref-command-reads-the-holders-of-the-module ($($bm.arguments))"
    $badKeys = @('Software\ASIO\Test%PATH%', 'Software\ASIO\Test!PATH!', 'Software\ASIO\Te"st', 'Software\ASIO\A&B', 'Software\ASIO\A|B',
                 'Software\ASIO\A^B', 'Software\ASIO\A<B', 'Software\ASIO\A>B', "Software\ASIO\A`tB", 'HKLM:\Software\ASIO\Test Card',
                 'Registry::HKEY_LOCAL_MACHINE\Software\ASIO\Test Card', '\')
    foreach ($k in $badKeys) { Throws { Get-IemBootPrefCommand -PrefKey $k @bootArgs } "boot-pref-command-refuses-the-key [$k]" }
    foreach ($n in @('Pref%x%', 'Pref!x!', 'Pref"x', 'Pref&x', 'Pref\', 'Pref\x')) {
        Throws { Get-IemBootPrefCommand -PrefKey 'Software\ASIO\Test Card' -System $sys32 -PrefName $n -PrefOriginal '64' -Module 'testcard.dll' -LogDir $logDir } "boot-pref-command-refuses-the-value-name [$n]"
    }
    foreach ($o in @('6 4', '64x', '0x40', '123456')) {
        Throws { Get-IemBootPrefCommand -PrefKey 'Software\ASIO\Test Card' -System $sys32 -PrefName 'PrefBuffSize' -PrefOriginal $o -Module 'testcard.dll' -LogDir $logDir } "boot-pref-command-refuses-the-original [$o]"
    }
    foreach ($m in @('test%x%.dll', 'test!x!.dll', 'te^st.dll', 'a&b.dll', 'a|b.dll', 'a"b.dll', "a`tb.dll", 'x\testcard.dll', 'testcard', 'testcard.exe')) {
        $e = ErrorOf { Get-IemBootPrefCommand -PrefKey 'Software\ASIO\Test Card' -System $sys32 -PrefName 'PrefBuffSize' -PrefOriginal '64' -Module $m -LogDir $logDir }
        Assert ($e -like "*module name*refused*" -or $e -like "*refused for the boot task's command line*") "boot-pref-command-refuses-the-module [$m] ($e)"
    }
    Throws { Get-IemBootPrefCommand -PrefKey 'Software\ASIO\Test Card' -System $sys32 -PrefName 'PrefBuffSize' -PrefOriginal '64' -PrefKind 'binary' -Module 'testcard.dll' -LogDir $logDir } 'boot-pref-command-refuses-another-kind'
    foreach ($d in @('C:\Program%x%Data\out', 'C:\Program!x!Data\out', 'tasks\out')) {
        Throws { Get-IemBootPrefCommand -PrefKey 'Software\ASIO\Test Card' -System $sys32 -PrefName 'PrefBuffSize' -PrefOriginal '64' -Module 'testcard.dll' -LogDir $d } "boot-pref-command-refuses-the-log-folder [$d]"
    }
    Throws { Get-IemBootPrefCommand -PrefKey 'Software\ASIO\Test Card' -System 'Windows\System32' -PrefName 'PrefBuffSize' -PrefOriginal '64' -Module 'testcard.dll' -LogDir $logDir } 'boot-pref-command-refuses-a-relative-system-folder'
    # The read-back's rules (Test-IemTaskReport), pure: a task as Register-IemTasks
    # makes it passes; each deviation is named. The boot task (-Boot) logs on S4U,
    # never restarts and never starts on demand; no task starts late, repeats,
    # waits a trigger delay or has a disabled trigger.
    $goodTask = [pscustomobject]@{ logon_type = 3; run_level = 1; time_limit = 'PT0S'; instances = 2; restart = '3xPT1M'; demand_start = $true
                                   late_start = $false; repeat = ''; delays = ''; disabled = 0; batteries = $true; idle_stop = $false
                                   hard_end = $false; priority = 4; sddl_ok = $true; sddl = 'D:' }
    $goodBoot = $goodTask.PSObject.Copy()
    $goodBoot.logon_type = 2
    $goodBoot.restart = '0x'
    $goodBoot.demand_start = $false
    $tr0 = Test-IemTaskReport -Report $goodTask -RunLevel 1
    $tr1 = Test-IemTaskReport -Report $goodBoot -RunLevel 1 -Boot
    Assert ($tr0.Count -eq 0 -and $tr1.Count -eq 0) "task-report-our-tasks-as-registered-pass ($($tr0 -join '; ') / $($tr1 -join '; '))"
    $bootCases = @(@('demand_start', $true, 'start on demand True'), @('late_start', $true, 'starts late*'), @('repeat', 'PT1H', 'repeats PT1H'),
                   @('restart', '3xPT1M', 'restart 3xPT1M'), @('logon_type', 3, 'logon type 3'), @('delays', 'PT30S', 'trigger delay PT30S'),
                   @('disabled', 1, '1 disabled trigger*'))
    foreach ($c in $bootCases) {
        $bad = $goodBoot.PSObject.Copy()
        $bad.($c[0]) = $c[1]
        $tb = Test-IemTaskReport -Report $bad -RunLevel 1 -Boot
        Assert ($tb.Count -eq 1 -and $tb[0] -clike $c[2]) "task-report-refuses-a-boot-task-with-$($c[0]) ($($tb -join '; '))"
    }
    $noDemand = $goodTask.PSObject.Copy()
    $noDemand.demand_start = $false
    $tn = Test-IemTaskReport -Report $noDemand -RunLevel 1
    Assert ($tn.Count -eq 1 -and $tn[0] -ceq 'start on demand False') "task-report-refuses-a-task-the-guard-cannot-start ($($tn -join '; '))"
    $ts = Test-IemTaskReport -Report $goodBoot -RunLevel 1
    Assert ($ts.Count -eq 3) "task-report-a-boot-task-is-no-ordinary-task ($($ts -join '; '))"
    # Its log as Get-IemBootstrapState reads it: the last run's block.
    $synth = Join-Path $base 'boot-pref-synthetic.log'
    [IO.File]::WriteAllLines($synth, [string[]]@('boot-pref d1 t1 add=0', '', 'HKEY_CURRENT_USER\Software\ASIO\Test Card', '    PrefBuffSize    REG_DWORD    0x20', '',
                                                  'boot-pref d2 t2 add=1', 'ERROR: x', ''))
    $sb = Get-IemBootPrefLog -Path $synth
    Assert ((@($sb) -join '|') -ceq 'boot-pref d2 t2 add=1|ERROR: x') "boot-pref-log-is-the-last-runs-block ($(@($sb) -join '|'))"
    $sn = Get-IemBootPrefLog -Path (Join-Path $base 'no-such-boot-pref.log')
    Assert ($sn.Count -eq 0) 'boot-pref-log-is-empty-before-the-first-run'
    Assert ((Format-IemTaskTime -Time ([datetime]'1899-12-30')) -ceq '') 'task-time-never-run-is-empty'
    Assert ((Format-IemTaskTime -Time (New-Object DateTime 2026, 10, 7, 21, 13, 39, ([DateTimeKind]::Utc))) -ceq '2026-10-07T21:13:39.0000000Z') 'task-time-is-utc'
    # Fast Startup (pure, #35 review of PR #40): on only with HiberbootEnabled 1
    # AND the hibernation file present; the venue PC has HiberbootEnabled 1 with
    # hibernation off, a full shutdown: no problem there. A fact that cannot be
    # read leaves it unknown ($null), never a problem.
    $fsCases = @(@(1, $true, $true), @(1, $false, $false), @(0, $true, $false), @(0, $false, $false), @($null, $false, $false),
                 @(1, $null, $null), @($null, $true, $null), @($null, $null, $null))
    foreach ($c in $fsCases) {
        $fs = Get-IemFastStartup -Hiberboot $c[0] -HiberFile $c[1]
        $named = ($fs.problem -cne '')
        Assert ("$($fs.active)" -ceq "$($c[2])" -and $named -eq ($c[2] -eq $true) -and "$($fs.hiberboot_enabled)" -ceq "$($c[0])" -and "$($fs.hiberfile_present)" -ceq "$($c[1])") "fast-startup-is-on-only-with-hiberboot-and-the-hibernation-file [$($c[0]), $($c[1])] ($($fs.active): $($fs.problem))"
    }
    $fsOn = Get-IemFastStartup -Hiberboot 1 -HiberFile $true
    Assert ($fsOn.problem -clike '*iemmixer-boot-pref*' -and $fsOn.problem -clike '*logon task*') "fast-startup-on-names-the-boot-task-it-skips ($($fsOn.problem))"
    $fsVenue = Get-IemFastStartup -Hiberboot 1 -HiberFile $false
    # What Register-IemTasks says about the boot task: a warning only while it is on, never a refusal.
    $bw = Get-IemBootTaskWarnings -FastStartup $fsOn
    Assert ($bw.Count -eq 1 -and $bw[0] -ceq $fsOn.problem) "boot-task-warns-of-fast-startup-that-is-on ($($bw -join ' | '))"
    foreach ($f in @($fsVenue, (Get-IemFastStartup -Hiberboot 0 -HiberFile $true))) {
        $bw = Get-IemBootTaskWarnings -FastStartup $f
        Assert ($bw.Count -eq 0) "boot-task-warns-of-nothing-else [$($f.hiberboot_enabled), $($f.hiberfile_present)] ($($bw -join ' | '))"
    }
    # A fact that could not be read is named as such (Get-IemFastStartupState's
    # error), never as Fast Startup being on.
    $fsUnread = [pscustomobject]@{ hiberboot_enabled = 1; hiberfile_present = $null; active = $null; problem = ''; error = 'the hibernation file: x' }
    $bw = Get-IemBootTaskWarnings -FastStartup $fsUnread
    Assert ($bw.Count -eq 1 -and $bw[0] -ceq 'Fast Startup could not be judged (the hibernation file: x): if it is on, a shutdown skips iemmixer-boot-pref') "boot-task-names-a-fast-startup-it-cannot-judge ($($bw -join ' | '))"
    # An unknown fact without a read error is named too (review of PR #41): HiberbootEnabled
    # absent while the hibernation file is there, or the file unknown while HiberbootEnabled is 1.
    foreach ($c in @(@($null, $true, 'HiberbootEnabled absent'), @(1, $null, 'the hibernation file not read'))) {
        $bw = Get-IemBootTaskWarnings -FastStartup (Get-IemFastStartup -Hiberboot $c[0] -HiberFile $c[1])
        Assert ($bw.Count -eq 1 -and $bw[0] -ceq ('Fast Startup could not be judged ({0}): if it is on, a shutdown skips iemmixer-boot-pref' -f $c[2])) "boot-task-names-an-unknown-fast-startup-fact $($c[2]) ($($bw -join ' | '))"
    }
    # Each unknown fact is named, with the read error where there is one (review of PR #41).
    $fsBoth = [pscustomobject]@{ hiberboot_enabled = $null; hiberfile_present = $null; active = $null; problem = ''; error = 'the hibernation file: x' }
    $bw = Get-IemBootTaskWarnings -FastStartup $fsBoth
    Assert ($bw.Count -eq 1 -and $bw[0] -ceq 'Fast Startup could not be judged (HiberbootEnabled absent, the hibernation file: x): if it is on, a shutdown skips iemmixer-boot-pref') "boot-task-names-every-unknown-fast-startup-fact ($($bw -join ' | '))"
    $fsHbErr = [pscustomobject]@{ hiberboot_enabled = $null; hiberfile_present = $true; active = $null; problem = ''; error = 'HiberbootEnabled: x' }
    $bw = Get-IemBootTaskWarnings -FastStartup $fsHbErr
    Assert ($bw.Count -eq 1 -and $bw[0] -ceq 'Fast Startup could not be judged (HiberbootEnabled: x): if it is on, a shutdown skips iemmixer-boot-pref') "boot-task-names-an-unreadable-hiberboot-as-unreadable-not-absent ($($bw -join ' | '))"
    $fsRead = [pscustomobject]@{ hiberboot_enabled = 1; hiberfile_present = $false; active = $false; problem = ''; error = '' }
    Assert ((Get-IemBootTaskWarnings -FastStartup $fsRead).Count -eq 0) 'boot-task-warns-of-nothing-when-both-facts-were-read-and-it-is-off'

    # ---- Register-IemTasks on the real Task Scheduler ----
    $prefArgs = @{ PrefKey = $regKey; PrefName = 'Pref'; PrefOriginal = '64'; Module = 'testcard.dll' }
    $sch = New-Object -ComObject 'Schedule.Service'
    $sch.Connect()
    $e = ErrorOf { Register-IemTasks -Root $root -AppExe $appExe -Folder '\iemmixer-test-none' -ElevatedRoot $elevated @prefArgs }
    Assert ($e -like '*iemmixer-StartREAPER is missing*') "tasks-need-the-existing-start-reaper-task ($e)"
    Throws { $sch.GetFolder('\iemmixer-test-none') } 'tasks-refused-before-any-folder-exists'
    Assert (-not (Test-Path -LiteralPath $elevated)) 'tasks-refused-before-the-elevated-folder'
    Throws { Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedRoot $elevated -PrefKey $regKey -PrefName 'Pref' -PrefOriginal '64x' -Module 'testcard.dll' } 'tasks-refuse-a-non-numeric-original'
    $e = ErrorOf { Register-IemTasks -Root $root -AppExe $appExe -Folder '\iemmixer-test-none' -ElevatedRoot $elevated -PrefKey $regKey -PrefName 'Pref' -PrefOriginal '64' -Module 'a|b.dll' }
    Assert ($e -like "*module name 'a|b.dll' refused*") "tasks-refuse-a-bad-driver-module-name ($e)"
    Throws { Register-IemTasks -Root ($root + '"') -AppExe $appExe -Folder $folder -ElevatedRoot $elevated @prefArgs } 'tasks-refuse-a-quote-in-a-path'
    # The boot task's command line refuses what cmd would read, before anything is written.
    $e = ErrorOf { Register-IemTasks -Root $root -AppExe $appExe -Folder '\iemmixer-test-none' -ElevatedRoot $elevated -PrefKey ($regKey + '!x!') -PrefName 'Pref' -PrefOriginal '64' -Module 'testcard.dll' }
    Assert ($e -like "*refused for the boot task's command line*") "tasks-refuse-a-key-the-boot-task-cannot-carry ($e)"
    Assert (-not (Test-Path -LiteralPath $elevated)) 'tasks-refused-boot-key-before-the-elevated-folder'
    # The kind comes from the value in this session's HKCU: a missing value (a
    # typo; reg.exe add would create it) is refused before anything is written.
    $e = ErrorOf { Register-IemTasks -Root $root -AppExe $appExe -Folder '\iemmixer-test-none' -ElevatedRoot $elevated -PrefKey $regKey -PrefName 'NoSuchValue' -PrefOriginal '64' -Module 'testcard.dll' }
    Assert ($e -like "*cannot be read in this session's HKCU*") "tasks-refuse-a-preference-they-cannot-read ($e)"
    Assert (-not (Test-Path -LiteralPath $elevated)) 'tasks-refused-missing-value-before-the-elevated-folder'

    Register-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'iemmixer-StartREAPER' `
        -Action (New-ScheduledTaskAction -Execute 'cmd.exe' -Argument '/c exit 0') `
        -Principal (New-ScheduledTaskPrincipal -UserId $me.name -LogonType Interactive -RunLevel Limited) | Out-Null
    $reports = Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedRoot $elevated @prefArgs
    $byName = @{}
    foreach ($r in $reports) { $byName[$r.task] = $r }
    Assert ((Sorted $byName.Keys) -ceq (Sorted @('iemmixer-guard', 'iemmixer-StartApp', 'iemmixer-probe', 'iemmixer-tuning', 'iemmixer-exclude', 'iemmixer-logon', 'iemmixer-boot-pref', 'iemmixer-StartREAPER'))) 'tasks-all-eight'
    foreach ($r in $reports) { Assert ($r.sddl_ok -and $r.problems.Count -eq 0) "tasks-$($r.task)-reads-back-with-our-descriptor" }
    # Fast Startup is named on the boot task's row, by this PC's facts (Get-IemFastStartupState), and on no other.
    $wantWarn = Get-IemBootTaskWarnings -FastStartup (Get-IemFastStartupState)
    Assert ((@($byName['iemmixer-boot-pref'].warnings) -join '|') -ceq (@($wantWarn) -join '|') -and
            @($reports | Where-Object { $_.task -cne 'iemmixer-boot-pref' -and @($_.warnings).Count -gt 0 }).Count -eq 0) "tasks-name-fast-startup-on-the-boot-task-only ($(@($byName['iemmixer-boot-pref'].warnings) -join ' | '))"

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
    Assert ($l.Actions[0].Arguments -like "*-File `"$entry`" -Root `"$root`" -TuningDir `"$etuning`" -Kind logon -PrefKey `"$regKey`" -PrefName `"Pref`" -PrefOriginal `"64`" -Module `"testcard.dll`"") "tasks-logon-runs-the-elevated-entry ($($l.Actions[0].Arguments))"
    $tu = Get-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'iemmixer-tuning'
    Assert ($tu.Actions[0].Arguments -like "*-File `"$entry`" -Root `"$root`" -TuningDir `"$etuning`" -Kind tuning" -and $tu.Actions[0].Execute -like '*\WindowsPowerShell\v1.0\powershell.exe' -and $tu.Actions[0].WorkingDirectory -eq $etasks) 'tasks-tuning-runs-the-elevated-entry-with-its-tuning-folder'
    # The boot task (#35): the preference back to its original at the system's
    # start, before any logon, as the user (S4U, no stored password), elevated
    # (its log lies in the admin-only out folder), and at no other moment.
    $bp = Get-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'iemmixer-boot-pref'
    $bps = $bp.Settings
    Assert ("$($bp.Principal.LogonType)" -eq 'S4U' -and "$($bp.Principal.RunLevel)" -eq 'Highest' -and $bp.Principal.UserId -like "*$(Split-Path -Leaf $me.name)") "tasks-boot-pref-runs-as-the-user-s4u-highest ($($bp.Principal.LogonType), $($bp.Principal.RunLevel), $($bp.Principal.UserId))"
    Assert (@($bp.Triggers).Count -eq 1 -and $bp.Triggers[0].CimClass.CimClassName -eq 'MSFT_TaskBootTrigger' -and $bp.Triggers[0].Enabled) 'tasks-boot-pref-fires-only-at-the-systems-start'
    Assert (-not $bps.AllowDemandStart -and -not $bps.StartWhenAvailable -and $bps.RestartCount -eq 0) 'tasks-boot-pref-never-on-demand-late-or-again'
    Assert ($bps.ExecutionTimeLimit -eq 'PT0S' -and "$($bps.MultipleInstances)" -eq 'IgnoreNew' -and -not $bps.AllowHardTerminate -and $bps.Priority -eq 4 -and
            -not $bps.DisallowStartIfOnBatteries -and -not $bps.StopIfGoingOnBatteries -and -not $bps.IdleSettings.StopOnIdleEnd) 'tasks-boot-pref-unbounded-ignorenew-never-ended-hard'
    $bootCmd = Get-IemBootPrefCommand -System ([Environment]::GetFolderPath('System')) -PrefKey $regKey -PrefName 'Pref' -PrefOriginal '64' -Module 'testcard.dll' -LogDir $eout
    Assert (@($bp.Actions).Count -eq 1 -and $bp.Actions[0].Execute -eq $bootCmd.exe -and $bp.Actions[0].Arguments -ceq $bootCmd.arguments -and
            $bp.Actions[0].WorkingDirectory -eq $etasks) "tasks-boot-pref-runs-reg-exe-through-cmd ($($bp.Actions[0].Execute) $($bp.Actions[0].Arguments))"
    Assert ($bootCmd.arguments -clike "*`"$eout\boot-pref.log`"*") 'tasks-boot-pref-logs-into-the-admin-only-out-folder'
    Throws { New-IemTaskDefinition -Scheduler $sch -User $me.name -RunLevel 0 -Exe 'cmd.exe' -AtLogon -AtBoot } 'task-definition-refuses-logon-and-boot-together'
    # The read-back sees what our tasks never have: a repetition, a trigger
    # delay, a disabled trigger (a task registered here by the cmdlets).
    $oddTriggers = @((New-ScheduledTaskTrigger -Once -At (Get-Date).AddDays(1) -RepetitionInterval (New-TimeSpan -Hours 1) -RepetitionDuration (New-TimeSpan -Days 1)),
                     (New-ScheduledTaskTrigger -AtLogOn -User $me.name))
    $oddTriggers[1].Delay = 'PT30S'
    $oddTriggers[1].Enabled = $false
    Register-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'iemmixer-test-odd' -Trigger $oddTriggers `
        -Action (New-ScheduledTaskAction -Execute 'cmd.exe' -Argument '/c exit 0') `
        -Principal (New-ScheduledTaskPrincipal -UserId $me.name -LogonType Interactive -RunLevel Limited) | Out-Null
    $odd = Get-IemTaskReport -Task $sch.GetFolder($folder).GetTask('iemmixer-test-odd') -UserSid $me.sid
    Assert ($odd.repeat -ceq 'PT1H' -and $odd.delays -ceq 'PT30S' -and $odd.disabled -eq 1 -and $odd.demand_start -and -not $odd.late_start) "task-report-reads-repetition-delay-and-disabled-triggers ($($odd.repeat), $($odd.delays), $($odd.disabled))"
    $oddBad = Test-IemTaskReport -Report $odd -RunLevel 0
    Assert ((@($oddBad) -join ';') -clike '*repeats PT1H*trigger delay PT30S*1 disabled trigger*') "task-report-refuses-them ($($oddBad -join '; '))"
    $sr = Get-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'iemmixer-StartREAPER'
    Assert ($sr.Actions[0].Execute -eq 'cmd.exe' -and $sr.Actions[0].Arguments -eq '/c exit 0') 'tasks-start-reaper-keeps-its-action'
    foreach ($n in @('iemmixer-StartREAPER', 'iemmixer-guard', 'iemmixer-exclude')) {
        $sd = $sch.GetFolder($folder).GetTask($n).GetSecurityDescriptor(4)
        Assert (Test-IemTaskSddl -Sddl $sd -UserSid $me.sid) "tasks-$n-descriptor-lets-the-user-run-it ($sd)"
    }
    # The elevated root: this module and the entry, the results and the tuning
    # folder, owned by Administrators and changeable only by them and SYSTEM.
    Assert ((Get-FileHash -LiteralPath (Join-Path $etasks 'IemPc.psm1')).Hash -eq (Get-FileHash -LiteralPath (Join-Path $here 'IemPc.psm1')).Hash) 'elevated-folder-holds-this-module'
    # ... and each of its siblings (#36), which the entry's IemPc.psm1 imports from that folder.
    foreach ($n in $pcNames) {
        Assert ((Get-FileHash -LiteralPath (Join-Path $etasks $n)).Hash -eq (Get-FileHash -LiteralPath (Join-Path $here $n)).Hash) "elevated-folder-holds-the-module-file [$n]"
    }
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
    foreach ($f in @(@($pcNames | ForEach-Object { Join-Path $etasks $_ }) + @($entry))) {
        $fa = Get-Acl -LiteralPath $f
        $fr = @($fa.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
        Assert ($fa.GetOwner([System.Security.Principal.SecurityIdentifier]).Value -eq 'S-1-5-32-544' -and $fr.Count -eq 3 -and @($fr | Where-Object { -not $_.IsInherited }).Count -eq 0) "elevated-file-owned-by-administrators-rules-inherited [$f]"
        $fb = Test-IemElevatedItem -Path $f -UserSid $me.sid
        Assert ($fb.Count -eq 0) "elevated-file-reads-back [$f] ($($fb -join '; '))"
    }
    $eb = Test-IemElevatedItem -Path $elevated -UserSid $me.sid
    Assert ($eb.Count -eq 0) "elevated-root-reads-back ($($eb -join '; '))"
    # A module file missing where the module was imported from (#36): Install-IemElevatedDir
    # reads them all before it writes, so it refuses and the tasks folder stays as it was.
    $pcCopy = Join-Path $base 'pc-copy'
    New-Item -ItemType Directory -Force -Path $pcCopy | Out-Null
    foreach ($f in $pcFiles) { Copy-Item -LiteralPath $f -Destination $pcCopy }
    $child = Join-Path $base 'missing-module-file.ps1'
    $childText = @'
param([string]$Dir, [string]$Root, [string]$Sid, [string]$Gone)
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $Dir 'IemPc.psm1') -Force
[IO.File]::Delete((Join-Path $Dir $Gone))
try { Install-IemElevatedDir -Root $Root -UserSid $Sid ; exit 0 } catch { [Console]::Error.WriteLine("refused: $_") ; exit 3 }
'@
    [IO.File]::WriteAllText($child, $childText)
    function TasksFolder { return ((@(Get-ChildItem -LiteralPath $etasks -File | Sort-Object Name | ForEach-Object { '{0}={1}@{2}' -f $_.Name, (Get-FileHash -LiteralPath $_.FullName).Hash, $_.LastWriteTimeUtc.Ticks })) -join ';') }
    $tasksBefore = TasksFolder
    $ErrorActionPreference = 'Continue'
    $said = (& powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $child -Dir $pcCopy -Root $elevated -Sid $me.sid -Gone 'IemPcState.psm1' 2>&1 | ForEach-Object { "$_" }) -join ' '
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    Assert ($code -eq 3 -and $said -like '*IemPcState.psm1*' -and (TasksFolder) -ceq $tasksBefore) "elevated-folder-refuses-a-missing-module-file-and-changes-nothing (exit $code; $said)"
    Throws { Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedRoot (Join-Path $root 'elevated') @prefArgs } 'tasks-refuse-an-elevated-root-inside-the-users-root'
    Throws { Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedRoot 'relative\elevated' @prefArgs } 'tasks-refuse-a-relative-elevated-root'
    # A descriptor loosened after registration (here Users may run the guard's
    # task) is put back by the next run: an update keeps a task's old descriptor,
    # so Register-IemTasks sets ours on every task, as it does on StartREAPER.
    $sch.GetFolder($folder).GetTask('iemmixer-guard').SetSecurityDescriptor("D:(A;;GRGX;;;$($me.sid))(A;;FA;;;BA)(A;;FA;;;SY)(A;;GRGX;;;BU)", 16)
    $sd = $sch.GetFolder($folder).GetTask('iemmixer-guard').GetSecurityDescriptor(4)
    Assert (-not (Test-IemTaskSddl -Sddl $sd -UserSid $me.sid)) "tasks-precondition-the-guard-task-loosened ($sd)"
    $again = Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedRoot $elevated @prefArgs
    Assert ($again.Count -eq 8 -and @($again | Where-Object { $_.problems.Count -gt 0 }).Count -eq 0) 'tasks-register-again-idempotent'
    $sd = $sch.GetFolder($folder).GetTask('iemmixer-guard').GetSecurityDescriptor(4)
    Assert (Test-IemTaskSddl -Sddl $sd -UserSid $me.sid) "tasks-register-again-restores-a-loosened-descriptor ($sd)"
    # Fast Startup that is on (given here with -FastStartup; by default this
    # PC's facts): the registration goes through, warns once, and names the
    # problem on the boot task's row only.
    $fsReports = Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedRoot $elevated @prefArgs -FastStartup $fsOn -WarningVariable fsWarned -WarningAction SilentlyContinue
    $fsByName = @{}
    foreach ($r in $fsReports) { $fsByName[$r.task] = $r }
    Assert ($fsReports.Count -eq 8 -and @($fsReports | Where-Object { $_.problems.Count -gt 0 }).Count -eq 0) 'tasks-register-with-fast-startup-on'
    Assert ((@($fsByName['iemmixer-boot-pref'].warnings) -join '|') -ceq $fsOn.problem -and
            @($fsReports | Where-Object { $_.task -cne 'iemmixer-boot-pref' -and @($_.warnings).Count -gt 0 }).Count -eq 0) "tasks-name-fast-startup-that-is-on-on-the-boot-task-only ($(@($fsByName['iemmixer-boot-pref'].warnings) -join ' | '))"
    Assert (@($fsWarned).Count -eq 1 -and "$($fsWarned[0])" -ceq $fsOn.problem) "tasks-warn-once-of-fast-startup-that-is-on ($(@($fsWarned) -join ' | '))"
    # The boot task writes the kind the value has (as the logon task keeps it).
    New-ItemProperty -LiteralPath $regKey -Name 'TextPref' -Value '64' -PropertyType String | Out-Null
    $tx = Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedRoot $elevated -PrefKey $regKey -PrefName 'TextPref' -PrefOriginal '64' -Module 'testcard.dll'
    $txa = (Get-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'iemmixer-boot-pref').Actions[0].Arguments
    Assert ($tx.Count -eq 8 -and $txa -clike '* /c:"    TextPref    REG_SZ    64" * /v "TextPref" /t REG_SZ /d 64 /f *') "tasks-boot-pref-takes-the-kind-of-the-value ($txa)"
    $back = Register-IemTasks -Root $root -AppExe $appExe -Folder $folder -ElevatedRoot $elevated @prefArgs
    Assert ($back.Count -eq 8 -and (Get-ScheduledTask -TaskPath '\iemmixer-test\' -TaskName 'iemmixer-boot-pref').Actions[0].Arguments -ceq $bootCmd.arguments) 'tasks-boot-pref-back-to-the-dword'

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
    Assert ((@($a1.reset) -contains 'bundles\own') -and (@($a1.reset) -contains 'bundles\own\y.txt')) "root-acl-resets-the-items-with-rules-of-their-own ($(@($a1.reset) -join ', '); $(@($a1.differed) -join '; '))"
    # What each item reset held, read after the root's DACL was set: y.txt only
    # its own rule for Everyone (own, reset before it, passes on the root's rules).
    $yHeld = @($a1.differed | Where-Object { $_ -like '*\own\y.txt:*' })
    Assert ($yHeld.Count -eq 1 -and $yHeld[0] -like '*\own\y.txt: a rule of its own for S-1-1-0') "root-acl-reports-what-each-reset-item-held ($(@($a1.differed) -join '; '))"
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
    Assert ($r.changed -and @($r.before).Count -gt 0) "root-acl-without-the-junction-closes-the-loosened-root ($(@($r.reset) -join ', '); $(@($r.differed) -join '; '))"
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

    # ---- the preference check's decision (pure): the guard's PrefCheck rule (#9 2026-09-28) ----
    $pa = Get-IemPrefAction -IsOriginal $true -Holders @('reaper.exe:11')
    Assert ($pa.action -ceq 'none' -and @($pa.holders).Count -eq 0) 'pref-action-at-the-original-is-none-whoever-holds-the-driver'
    $pa = Get-IemPrefAction -IsOriginal $true -Holders $null -Reaper @('reaper:11')
    Assert ($pa.action -ceq 'none' -and @($pa.holders).Count -eq 0) 'pref-action-at-the-original-is-none-with-unreadable-holders'
    $pa = Get-IemPrefAction -IsOriginal $false -Holders @()
    Assert ($pa.action -ceq 'restore' -and @($pa.holders).Count -eq 0) 'pref-action-restores-while-nothing-holds-the-driver'
    $pa = Get-IemPrefAction -IsOriginal $false -Holders @('reaper.exe:11')
    Assert ($pa.action -ceq 'held' -and (@($pa.holders) -join ',') -ceq 'reaper.exe:11') 'pref-action-never-writes-under-reaper'
    $pa = Get-IemPrefAction -IsOriginal $false -Holders @('spike.exe:99', 'reaper.exe:11')
    Assert ($pa.action -ceq 'held' -and (@($pa.holders) -join ',') -ceq 'spike.exe:99,reaper.exe:11') 'pref-action-never-writes-under-any-holder'
    $pa = Get-IemPrefAction -IsOriginal $false -Holders $null -Reaper @('reaper:11')
    Assert ($pa.action -ceq 'held' -and (@($pa.holders) -join ',') -ceq 'reaper:11') 'pref-action-assumes-a-running-reaper-holds-when-the-holders-are-unreadable'
    $pa = Get-IemPrefAction -IsOriginal $false -Holders $null
    Assert ($pa.action -ceq 'restore' -and @($pa.holders).Count -eq 0) 'pref-action-restores-unreadable-holders-without-reaper'

    # ---- the preference and the elevated tasks' body ----
    # A driver module no process holds, and one every process holds.
    $free = 'iemmixer-free-' + $id + '.dll'
    $heldModule = 'kernel32.dll'
    Set-ItemProperty -LiteralPath $regKey -Name 'Pref' -Value 32 -Type DWord
    $noTuning = Join-Path $base 'no-tuning'
    $lg = Invoke-IemTaskRequest -Kind logon -Root $root -OutDir $eout -TuningDir $noTuning -PrefKey $regKey -PrefName 'Pref' -PrefOriginal '64' -Module $free
    $pv = Get-IemPref -Key $regKey -Name 'Pref'
    Assert ($lg.ok -and $pv.value -eq 64 -and $pv.kind -eq 'DWord' -and $lg.result.pref.attempts -eq 1 -and $lg.result.pref.action -ceq 'restore' -and $lg.result.tuning -eq 'absent') 'logon-restores-the-preference-keeping-its-kind'
    Assert ((Get-Content -LiteralPath (Join-Path $eout 'logon.result.json') -Raw | ConvertFrom-Json).ok) 'logon-writes-its-result-in-the-admin-only-folder'
    Assert (-not (Test-Path -LiteralPath (Join-Path $root 'guard'))) 'the-elevated-task-writes-nothing-in-the-users-root'
    $r0 = Restore-IemPref -Key $regKey -Name 'Pref' -Original '64' -Module $heldModule
    Assert ($r0.ok -and $r0.attempts -eq 0 -and $r0.action -ceq 'none') 'pref-at-its-original-is-not-written'
    # Not the original while a process holds the driver module (REAPER started
    # at 32 after a power loss): never written, the value and the holders named.
    Set-ItemProperty -LiteralPath $regKey -Name 'Pref' -Value 32 -Type DWord
    $ph = Restore-IemPref -Key $regKey -Name 'Pref' -Original '64' -Module $heldModule
    Assert ($ph.action -ceq 'held' -and -not $ph.ok -and $ph.attempts -eq 0 -and $ph.before -ceq '32' -and $ph.after -ceq '32' -and @($ph.holders).Count -gt 1 -and (Get-IemPref -Key $regKey -Name 'Pref').value -eq 32) 'pref-never-written-while-a-process-holds-the-driver-module'
    $lh = Invoke-IemTaskRequest -Kind logon -Root $root -OutDir $eout -TuningDir $noTuning -PrefKey $regKey -PrefName 'Pref' -PrefOriginal '64' -Module $heldModule
    $lj = Get-Content -LiteralPath (Join-Path $eout 'logon.result.json') -Raw | ConvertFrom-Json
    Assert ($lh.ok -and $lj.ok -and $lj.kind -ceq 'logon' -and $lj.at -and $lj.result.pref.action -ceq 'held' -and $lj.result.pref.before -ceq '32' -and @($lj.result.pref.holders).Count -gt 1 -and (Get-IemPref -Key $regKey -Name 'Pref').value -eq 32) 'logon-leaves-a-held-preference-and-names-it-for-the-guard'
    Set-ItemProperty -LiteralPath $regKey -Name 'Pref' -Value 64 -Type DWord
    New-ItemProperty -LiteralPath $regKey -Name 'Text' -Value '32' -PropertyType String | Out-Null
    $rt = Restore-IemPref -Key $regKey -Name 'Text' -Original '064' -Module $free
    $tv = Get-IemPref -Key $regKey -Name 'Text'
    Assert ($rt.ok -and $rt.action -ceq 'restore' -and $tv.raw -ceq '064' -and $tv.kind -eq 'String') 'pref-restores-the-raw-text-of-a-string'
    Throws { Restore-IemPref -Key $regKey -Name 'Pref' -Original '6 4' -Module $free } 'pref-refuses-a-non-numeric-original'
    $e = ErrorOf { Restore-IemPref -Key $regKey -Name 'Pref' -Original '64' -Module 'a|b.dll' }
    Assert ($e -like "*module name 'a|b.dll' refused*") "pref-refuses-a-bad-driver-module-name ($e)"
    Assert ((ConvertTo-IemHkcuPath -Key 'Software\ASIO\Test Card') -ceq 'HKCU:\Software\ASIO\Test Card') 'pref-site-key-is-under-hkcu'
    $nk = Invoke-IemTaskRequest -Kind logon -Root $root -OutDir $eout -TuningDir $noTuning -PrefKey $regKey -PrefName 'NoSuchValue' -PrefOriginal '64' -Module $free
    Assert (-not $nk.ok -and $nk.error) 'logon-reports-a-missing-value'
    $nm = Invoke-IemTaskRequest -Kind logon -Root $root -OutDir $eout -TuningDir $noTuning -PrefKey $regKey -PrefName 'Pref' -PrefOriginal '64'
    Assert (-not $nm.ok -and $nm.error -like '*-Module*') "logon-needs-the-driver-module ($($nm.error))"

    # The boot task's action as the task starts it (#35; here as this elevated
    # user, not at a boot): the preference left at 32 by a parked engine is
    # written back to the original, its kind kept, and each run appends its
    # block (header, reg.exe's read-back) to the log in the out folder. The
    # task itself never starts on demand, so its registered action is run
    # directly; a run that does not end within 30 s fails the test (nothing is
    # ended, I8).
    function Invoke-BootPrefAction($Action) {
        $psi = New-Object Diagnostics.ProcessStartInfo
        $psi.FileName = [string]$Action.Path
        $psi.Arguments = [string]$Action.Arguments
        $psi.WorkingDirectory = [string]$Action.WorkingDirectory
        $psi.UseShellExecute = $false
        $psi.CreateNoWindow = $true
        $proc = [Diagnostics.Process]::Start($psi)
        if (-not $proc.WaitForExit(30000)) { throw 'the boot task action did not end within 30 s' }
        return $proc.ExitCode
    }
    $bootLog = Join-Path $eout 'boot-pref.log'
    $bootHolders = Join-Path $eout 'boot-pref.holders'
    # Lines tasklist writes for a process holding the module ("image","pid",...).
    function HolderRows($Path) { return ,@([IO.File]::ReadAllLines($Path) | Where-Object { $_.StartsWith('"') }) }
    $bootAction = @($sch.GetFolder($folder).GetTask('iemmixer-boot-pref').Definition.Actions)[0]
    Set-ItemProperty -LiteralPath $regKey -Name 'Pref' -Value 32 -Type DWord
    $bx1 = Invoke-BootPrefAction $bootAction
    $bv = Get-IemPref -Key $regKey -Name 'Pref'
    Assert ($bx1 -eq 0 -and $bv.value -eq 64 -and $bv.kind -eq 'DWord') "boot-pref-action-writes-the-original-as-a-dword (exit $bx1, $($bv.raw) $($bv.kind))"
    $blk = Get-IemBootPrefLog -Path $bootLog
    Assert (@($blk).Count -eq 3 -and $blk[0] -clike 'boot-pref * add=0' -and $blk[1] -clike 'HKEY_CURRENT_USER\Software\iemmixer-iempc-test-*' -and
            $blk[2] -cmatch '^\s+Pref\s+REG_DWORD\s+0x40\s*$') "boot-pref-action-logs-its-read-back ($(@($blk) -join ' | '))"
    # It wrote only after reading the module's holders: none (no process loads testcard.dll).
    Assert (Test-Path -LiteralPath $bootHolders -PathType Leaf) 'boot-pref-action-reads-the-holders-before-it-writes'
    $freeRows = HolderRows $bootHolders
    Assert ($freeRows.Count -eq 0) "boot-pref-action-finds-no-holder-of-a-module-no-process-loads ($($freeRows -join ' | '))"
    # At the original (a normal boot) it writes nothing, reads no holders and says so.
    Remove-Item -LiteralPath $bootHolders -Force
    $bx2 = Invoke-BootPrefAction $bootAction
    $heads = @([IO.File]::ReadAllLines($bootLog) | Where-Object { $_.StartsWith('boot-pref ') })
    $blk2 = Get-IemBootPrefLog -Path $bootLog
    Assert ($bx2 -eq 0 -and $heads.Count -eq 2 -and $blk2[0] -clike 'boot-pref * add=none' -and (Get-IemPref -Key $regKey -Name 'Pref').value -eq 64) "boot-pref-action-at-the-original-writes-nothing ($(@($blk2) -join ' | '))"
    Assert (-not (Test-Path -LiteralPath $bootHolders)) 'boot-pref-action-at-the-original-reads-no-holders'
    Assert ($bootAction.Arguments.IndexOf($root, [StringComparison]::OrdinalIgnoreCase) -lt 0 -and $bootAction.Arguments.IndexOf($eout, [StringComparison]::OrdinalIgnoreCase) -ge 0) 'boot-pref-action-writes-only-into-the-admin-only-out-folder'
    # A task started late, while a process holds the driver module (REAPER on
    # the card at 32, here kernel32.dll, which every process holds): nothing is
    # written, the run says add=held and succeeds; the logon task and the guard
    # name it (its own log folder here).
    $heldLog = Join-Path $base 'boot-pref-held'
    New-Item -ItemType Directory -Force -Path $heldLog | Out-Null
    $hc = Get-IemBootPrefCommand -System ([Environment]::GetFolderPath('System')) -PrefKey $regKey -PrefName 'Pref' -PrefOriginal '64' -Module $heldModule -LogDir $heldLog
    $heldAction = [pscustomobject]@{ Path = $hc.exe; Arguments = $hc.arguments; WorkingDirectory = $etasks }
    Set-ItemProperty -LiteralPath $regKey -Name 'Pref' -Value 32 -Type DWord
    $hx = Invoke-BootPrefAction $heldAction
    $hv = Get-IemPref -Key $regKey -Name 'Pref'
    $hb = Get-IemBootPrefLog -Path (Join-Path $heldLog 'boot-pref.log')
    $heldRows = HolderRows (Join-Path $heldLog 'boot-pref.holders')
    Assert ($hx -eq 0 -and $hv.value -eq 32 -and @($hb).Count -eq 3 -and $hb[0] -clike 'boot-pref * add=held' -and $hb[2] -cmatch '^\s+Pref\s+REG_DWORD\s+0x20\s*$' -and
            $heldRows.Count -gt 1) "boot-pref-action-never-writes-while-a-process-holds-the-driver-module (exit $hx, $($hv.raw); $(@($hb) -join ' | '); $($heldRows.Count) holders)"
    # A holder list that cannot be written (its path taken by a folder) writes
    # nothing either: add=unread, exit 2 (the task's last result names it).
    $unreadLog = Join-Path $base 'boot-pref-unread'
    New-Item -ItemType Directory -Force -Path (Join-Path $unreadLog 'boot-pref.holders') | Out-Null
    $uc = Get-IemBootPrefCommand -System ([Environment]::GetFolderPath('System')) -PrefKey $regKey -PrefName 'Pref' -PrefOriginal '64' -Module 'testcard.dll' -LogDir $unreadLog
    $unreadAction = [pscustomobject]@{ Path = $uc.exe; Arguments = $uc.arguments; WorkingDirectory = $etasks }
    $ux = Invoke-BootPrefAction $unreadAction
    $uv = Get-IemPref -Key $regKey -Name 'Pref'
    $ub = Get-IemBootPrefLog -Path (Join-Path $unreadLog 'boot-pref.log')
    Assert ($ux -eq 2 -and $uv.value -eq 32 -and @($ub).Count -eq 3 -and $ub[0] -clike 'boot-pref * add=unread') "boot-pref-action-writes-nothing-without-a-holder-list (exit $ux, $($uv.raw); $(@($ub) -join ' | '))"
    # A list tasklist wrote but findstr cannot open (reading it denied to this
    # user, writing allowed): findstr exits 1 then, as for "no holder", so the
    # add waits for findstr /v's proof that the list was read, which cannot
    # come: nothing is written either (its own log folder here).
    $deniedLog = Join-Path $base 'boot-pref-denied'
    New-Item -ItemType Directory -Force -Path $deniedLog | Out-Null
    $deniedList = Join-Path $deniedLog 'boot-pref.holders'
    [IO.File]::WriteAllText($deniedList, '')
    $deniedAcl = [IO.File]::GetAccessControl($deniedList)
    $denyRead = New-Object System.Security.AccessControl.FileSystemAccessRule((New-Object System.Security.Principal.SecurityIdentifier($me.sid)),
        [System.Security.AccessControl.FileSystemRights]::ReadData, [System.Security.AccessControl.AccessControlType]::Deny)
    $deniedAcl.AddAccessRule($denyRead)
    [IO.File]::SetAccessControl($deniedList, $deniedAcl)
    $dc = Get-IemBootPrefCommand -System ([Environment]::GetFolderPath('System')) -PrefKey $regKey -PrefName 'Pref' -PrefOriginal '64' -Module 'testcard.dll' -LogDir $deniedLog
    $deniedAction = [pscustomobject]@{ Path = $dc.exe; Arguments = $dc.arguments; WorkingDirectory = $etasks }
    $dx = Invoke-BootPrefAction $deniedAction
    $dv = Get-IemPref -Key $regKey -Name 'Pref'
    $db = Get-IemBootPrefLog -Path (Join-Path $deniedLog 'boot-pref.log')
    $deniedLength = (Get-Item -LiteralPath $deniedList).Length
    [void]$deniedAcl.RemoveAccessRule($denyRead)
    [IO.File]::SetAccessControl($deniedList, $deniedAcl)
    Assert ($deniedLength -gt 0) "boot-pref-denied-precondition-tasklist-wrote-the-list ($deniedLength bytes)"
    Assert ($dx -eq 2 -and $dv.value -eq 32 -and @($db).Count -eq 3 -and $db[0] -clike 'boot-pref * add=unread') "boot-pref-action-writes-nothing-when-findstr-cannot-read-the-list (exit $dx, $($dv.raw); $(@($db) -join ' | '))"
    Set-ItemProperty -LiteralPath $regKey -Name 'Pref' -Value 64 -Type DWord
    # A string preference: compared and written as REG_SZ, kind kept (its own log folder here).
    $textLog = Join-Path $base 'boot-pref-text'
    New-Item -ItemType Directory -Force -Path $textLog | Out-Null
    Set-ItemProperty -LiteralPath $regKey -Name 'Text' -Value '32' -Type String
    $tc = Get-IemBootPrefCommand -System ([Environment]::GetFolderPath('System')) -PrefKey $regKey -PrefName 'Text' -PrefOriginal '064' -PrefKind 'text' -Module 'testcard.dll' -LogDir $textLog
    $textAction = [pscustomobject]@{ Path = $tc.exe; Arguments = $tc.arguments; WorkingDirectory = $etasks }
    $tx1 = Invoke-BootPrefAction $textAction
    $tv1 = Get-IemPref -Key $regKey -Name 'Text'
    $tb1 = Get-IemBootPrefLog -Path (Join-Path $textLog 'boot-pref.log')
    Assert ($tx1 -eq 0 -and $tv1.raw -ceq '064' -and $tv1.kind -eq 'String' -and $tb1[0] -clike 'boot-pref * add=0' -and $tb1[2] -cmatch '^\s+Text\s+REG_SZ\s+064\s*$') "boot-pref-action-writes-a-string-as-a-string ($(@($tb1) -join ' | '))"
    $tx2 = Invoke-BootPrefAction $textAction
    $tb2 = Get-IemBootPrefLog -Path (Join-Path $textLog 'boot-pref.log')
    Assert ($tx2 -eq 0 -and $tb2[0] -clike 'boot-pref * add=none') "boot-pref-action-at-a-string-original-writes-nothing ($(@($tb2) -join ' | '))"

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

    # S1c's tuning module is imported only from an admin-owned, admin-only folder,
    # its store (#34: IemTuning imports IemTuningStore.psm1 from its own folder) read
    # back the same way.
    $fakeTuning = @'
$global:IemFakeTempAtImport = $env:TEMP
function Get-IemTuningState { param([string]$ProfilePath) return 'fake-state' }
function Enter-IemTuningMode { param([string]$ProfilePath) return 'fake-enter' }
function Exit-IemTuningMode { param([string]$ProfilePath) return 'fake-exit' }
function Invoke-IemTuningApply { param([string]$ProfilePath, [int]$Tier) return ('fake-apply-{0}' -f $Tier) }
'@
    $tmod = Join-Path $etuning 'IemTuning.psm1'
    $tstore = Join-Path $etuning 'IemTuningStore.psm1'
    $tprof = Join-Path $etuning 'profile.json'
    function Write-FakeTuning {
        foreach ($p in @($tmod, $tstore, $tprof)) { if (Test-Path -LiteralPath $p) { Remove-Item -LiteralPath $p -Force } }
        [IO.File]::WriteAllText($tmod, $fakeTuning)
        [IO.File]::WriteAllText($tstore, "function Get-IemFakeStore { 'fake-store' }")
        [IO.File]::WriteAllText($tprof, '{}')
        foreach ($p in @($tmod, $tstore, $tprof)) { Set-IemAdminsOwner -Path $p }
    }
    Write-FakeTuning
    $savedTemp = @($env:TEMP, $env:TMP)
    $global:IemFakeTempAtImport = ''
    Assert ((Invoke-IemTuningVerb -Verb 'state' -TuningDir $etuning) -ceq 'fake-state') 'tuning-imports-an-admin-only-module'
    # The import ran with TEMP and TMP at the admin-only <elevated root>\temp (#15: IemTuning's Add-Type):
    # the fake module read TEMP while it was imported.
    $etemp = Join-Path (Split-Path -Parent $etuning) 'temp'
    $tb = Test-IemElevatedItem -Path $etemp -UserSid $me.sid
    Assert ($global:IemFakeTempAtImport -eq $etemp -and $env:TMP -eq $etemp -and $tb.Count -eq 0) "tuning-imports-with-the-admin-only-temp ($global:IemFakeTempAtImport; $($tb -join '; '))"
    Assert ((Invoke-IemTuningVerb -Verb 'apply-tier2' -TuningDir $etuning) -ceq 'fake-apply-2') 'tuning-apply-tier2-passes-tier-2'
    [IO.File]::WriteAllText((Join-Path $td 'tuning.request.json'), '{"id":"t-5","verb":"enter"}')
    $tr = Invoke-IemTaskRequest -Kind tuning -Root $root -OutDir $eout -TuningDir $etuning
    Assert ($tr.ok -and $tr.result -ceq 'fake-enter') 'tuning-request-runs-the-verified-module'
    foreach ($p in @($tmod, $tstore, $tprof)) {
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
    # A tuning folder from before #34 holds no IemTuningStore.psm1: its IemTuning needs
    # none, so the verb runs (a newer IemTuning without its store fails its own import).
    Remove-Item -LiteralPath $tstore -Force
    Assert ((Invoke-IemTuningVerb -Verb 'state' -TuningDir $etuning) -ceq 'fake-state') 'tuning-imports-a-module-from-before-the-store'
    # Anything else by the store's name is read back, a dangling link too (its
    # target gone): refused, never followed.
    $storeTarget = Join-Path $base 'store-target'
    New-Item -ItemType Directory -Force -Path $storeTarget | Out-Null
    New-Item -ItemType Junction -Path $tstore -Value $storeTarget | Out-Null
    [IO.Directory]::Delete($storeTarget)
    $e = ErrorOf { Invoke-IemTuningVerb -Verb 'state' -TuningDir $etuning }
    Assert ($e -like '*tuning module refused*junction or a link*') "tuning-refuses-a-dangling-link-by-the-stores-name ($e)"
    [IO.Directory]::Delete($tstore)
    Write-FakeTuning
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
    foreach ($p in @($tmod, $tstore, $tprof)) { Remove-Item -LiteralPath $p -Force }
    $env:TEMP = $savedTemp[0]
    $env:TMP = $savedTemp[1]

    # The generated entry, as the task runs it (its results in tasks\out).
    [IO.File]::WriteAllText((Join-Path $td 'tuning.request.json'), '{"id":"t-4","verb":"exit"}')
    & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $entry -Kind tuning -Root $root -TuningDir $noTuning | Out-Null
    $code = $LASTEXITCODE
    $res = Get-Content -LiteralPath (Join-Path $eout 'tuning.result.json') -Raw | ConvertFrom-Json
    Assert ($code -eq 0 -and $res.ok -and $res.id -ceq 't-4') "elevated-entry-runs-a-request (exit $code)"
    Assert (-not (Test-Path -LiteralPath (Join-Path $td 'tuning.result.json'))) 'elevated-entry-writes-no-result-into-the-users-root'
    # Its IemPc.psm1 imports the siblings from the tasks folder only (#36): with one
    # missing there the entry refuses (exit 2), names it, and answers nothing.
    $sib = Join-Path $etasks 'IemPcPref.psm1'
    $aside = Join-Path $etasks 'IemPcPref.aside'
    [IO.File]::Move($sib, $aside)
    Remove-Item -LiteralPath (Join-Path $eout 'tuning.result.json') -Force
    [IO.File]::WriteAllText((Join-Path $td 'tuning.request.json'), '{"id":"t-5","verb":"exit"}')
    $ErrorActionPreference = 'Continue'
    $said = (& powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $entry -Kind tuning -Root $root -TuningDir $noTuning 2>&1 | ForEach-Object { "$_" }) -join ' '
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    [IO.File]::Move($aside, $sib)
    Assert ($code -eq 2 -and $said -like '*IemPcPref.psm1*' -and -not (Test-Path -LiteralPath (Join-Path $eout 'tuning.result.json'))) "elevated-entry-refuses-without-a-sibling-in-its-folder (exit $code; $said)"
    & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $entry -Kind tuning -Root $root -TuningDir $noTuning | Out-Null
    $code = $LASTEXITCODE
    $res = Get-Content -LiteralPath (Join-Path $eout 'tuning.result.json') -Raw | ConvertFrom-Json
    Assert ($code -eq 0 -and $res.ok -and $res.id -ceq 't-5') "elevated-entry-runs-again-with-its-siblings (exit $code)"

    # ---- bootstrap state (read only) ----
    $bs = Get-IemBootstrapState -Root $root -Module 'iemmixer-no-such-module.dll' -PrefKey $regKey -PrefName 'Pref' -AppImage 'iemmixer-no-such-app.exe' -Folder $folder -FirewallRule $ruleName -ElevatedRoot $elevated
    Assert ($bs.holders.Count -eq 0 -and $bs.app -eq 0 -and $bs.reaper -eq 0 -and $bs.pref.value -eq 64) 'bootstrap-state-holders-processes-and-preference'
    Assert (@($bs.tasks | Where-Object { -not $_.exists -or -not $_.sddl_ok }).Count -eq 0 -and @($bs.tasks).Count -eq 8) 'bootstrap-state-our-tasks-with-their-descriptors'
    # The boot task: never run by Task Scheduler here (no boot, no start on
    # demand), so no last run and "has not yet run" (0x41303); its log holds the
    # direct runs above, the last one's block named.
    $bst = @($bs.tasks | Where-Object { $_.task -ceq 'iemmixer-boot-pref' })
    Assert ($bst.Count -eq 1 -and $bst[0].last_run -ceq '' -and $bst[0].last_result -eq 267011) "bootstrap-state-names-the-boot-task-and-its-last-result ($($bst[0].last_run), $($bst[0].last_result))"
    Assert (@($bs.boot_pref.log).Count -eq 3 -and $bs.boot_pref.log[0] -clike 'boot-pref * add=none' -and $bs.boot_pref.log[2] -cmatch 'REG_DWORD\s+0x40' -and -not $bs.boot_pref.log_error) "bootstrap-state-shows-the-boot-tasks-last-logged-run ($(@($bs.boot_pref.log) -join ' | '))"
    # Fast Startup turns a shutdown into a hibernation of the system session: the
    # next start fires no boot trigger. The state names both facts, each read
    # without a guess (HiberbootEnabled; hiberfil.sys listed at the system
    # volume's root), and
    # judges them as Get-IemFastStartup does (a problem only when it is on).
    $hb = $null
    try { $hb = (Get-Item -LiteralPath 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Power').GetValue('HiberbootEnabled', $null) } catch { $hb = $null }
    $fs = $bs.fast_startup
    Assert ("$($fs.hiberboot_enabled)" -ceq "$hb" -and $fs.hiberfile_present -is [bool] -and $fs.error -ceq '') "bootstrap-state-reads-fast-startup ($(ConvertTo-Json -InputObject $fs -Compress))"
    $fsWant = Get-IemFastStartup -Hiberboot $hb -HiberFile $fs.hiberfile_present
    Assert ("$($fs.active)" -ceq "$($fsWant.active)" -and $fs.problem -ceq $fsWant.problem) "bootstrap-state-judges-fast-startup-by-both-facts ($($fs.active))"
    Assert ($bs.boot_pref.path -ceq (Join-Path $eout 'boot-pref.log')) 'bootstrap-state-reads-the-boot-log-in-the-elevated-root'
    Assert ($bs.root.acl_ok -and $bs.firewall.present -and -not $bs.firewall.ok) "bootstrap-state-root-and-the-disabled-test-rule ($(@($bs.root.problems) -join '; '))"
    # An item below the root with a rule of its own: the state sees it, and the
    # next Set-IemRootAcl resets that item only.
    $loose = Join-Path $root 'bundles\x.txt'
    $looseAcl = [IO.File]::GetAccessControl($loose)
    $looseAcl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($everyone, 'Read', 'Allow')))
    [IO.File]::SetAccessControl($loose, $looseAcl)
    $bs = Get-IemBootstrapState -Root $root -Module 'iemmixer-no-such-module.dll' -PrefKey $regKey -PrefName 'Pref' -AppImage 'iemmixer-no-such-app.exe' -Folder $folder -FirewallRule $ruleName -ElevatedRoot $elevated
    Assert (-not $bs.root.acl_ok -and @($bs.root.problems | Where-Object { $_ -like '*x.txt*S-1-1-0*' }).Count -eq 1) "bootstrap-state-sees-an-item-below-the-root-with-a-rule-of-its-own ($(@($bs.root.problems) -join '; '))"
    $fix = Set-IemRootAcl -Root $root
    Assert ($fix.changed -and (@($fix.reset) -join ',') -ceq 'bundles\x.txt') "root-acl-resets-only-the-item-that-differs ($(@($fix.reset) -join ','))"
    Assert ((Get-IemModuleHolders -Module 'kernel32.dll').Count -gt 1) 'module-holders-lists-processes'
    Throws { Get-IemModuleHolders -Module 'a|b.dll' } 'module-holders-refuse-a-bad-name'
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
    Remove-Item -LiteralPath $regKey -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $base -Recurse -Force -ErrorAction SilentlyContinue
}
# Install-IemTuning (#15) in a process of its own: it imports S1c's IemTuning.psm1.
& powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File (Join-Path $here 'Test-IemTuningInstall.ps1')
if ($LASTEXITCODE -ne 0) { throw "FAILED: Test-IemTuningInstall.ps1 (exit $LASTEXITCODE)" }
# The bootstrap's admin-only stage (#15), the script iempc.py composes, in a process of its own.
& powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File (Join-Path $here 'Test-IemStage.ps1')
if ($LASTEXITCODE -ne 0) { throw "FAILED: Test-IemStage.ps1 (exit $LASTEXITCODE)" }
# iempc roster-install's PC bodies (#58), the scripts iempc_roster.py composes, in a process of its own.
& powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File (Join-Path $here 'Test-IemRoster.ps1')
if ($LASTEXITCODE -ne 0) { throw "FAILED: Test-IemRoster.ps1 (exit $LASTEXITCODE)" }
Write-Host 'Test-IemPc: all passed'
