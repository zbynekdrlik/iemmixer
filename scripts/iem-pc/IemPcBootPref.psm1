#Requires -Version 5.1
# The boot task's command line (#35; split out of IemPc.psm1 by
# responsibility, #36): Get-IemBootPrefCommand builds the native cmd.exe line
# of iemmixer-boot-pref (reg.exe, findstr.exe and tasklist.exe, never
# PowerShell) that Register-IemTasks registers.
# IemPc.psm1 imports this module from its own folder (-Global, as IemTuning
# imports IemTuningStore, #34), so it goes wherever IemPc.psm1 goes: the
# bundle, the bootstrap's admin-only stage, <elevated root>\tasks and the
# cutover's and the ssh shell undo's folders. Its functions reach the other
# IemPc modules' through the global session state. Nothing is ended by force
# (I8). No site value lives here (P6).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The boot task's log, its first query and its read of the driver module's
# holders, in <elevated root>\tasks\out (#35).
$script:BootPrefLog = 'boot-pref.log'
$script:BootPrefBefore = 'boot-pref.before'
$script:BootPrefHolders = 'boot-pref.holders'
# A driver module's name, as the [card] module names it (no path).
$script:ModulePattern = '^[^\\/:*?"<>|]+\.dll$'

# ---- the boot task's command line (#35) ----

function ConvertTo-IemRegExeKey {
    # The site's key (`Software\...`, relative to HKCU, or HKCU:\..., or
    # Registry::HKEY_CURRENT_USER\...) as reg.exe names it: HKCU\... . The
    # boot task writes the user's hive only; any other hive is refused.
    param([Parameter(Mandatory)][string]$Key)
    $k = $Key
    if ($k -match '^HKCU:') {
        $k = $k.Substring(5)
    } elseif ($k -match '^Registry::(HKEY_CURRENT_USER|HKCU)\\') {
        $k = $k.Substring($k.IndexOf('\') + 1)
    } elseif ($k.Contains(':')) {
        throw "key '$Key' refused (the boot task writes under HKCU only)"
    }
    $k = $k.Trim('\')
    if (-not $k) { throw "key '$Key' refused (no key below HKCU)" }
    return 'HKCU\' + $k
}

function Get-IemBootPrefCommand {
    # The boot task's action (#35): at the system's start the preference is
    # read and, only when it is not the original (a parked engine, a power
    # loss or a hard kill left 32) and no process holds the driver module,
    # written back, then read back. The task starts ~4 s after the boot,
    # before the desktop session's logon (the PC logs on by itself ~3 s
    # later) and well before REAPER, which the predecessor's autostart starts
    # ~16 s after the boot (#35, the PC 2026-10-06). The write never relies
    # on that timing (review of PR #40): tasklist.exe /m <module> /fo csv /nh
    # writes the module's holders into <log dir>\boot-pref.holders right
    # before the add, and any line starting with a quote (a holder's CSV row)
    # skips the add, as the logon task and the guard never write under a
    # holder (I2). A task started late under REAPER thus writes nothing
    # (add=held, exit 0) and REAPER runs at 32 until it quits, named by the
    # logon task and the guard. The add runs only on proof that the list was
    # read and names no holder: no quote line (findstr /b) AND a line without
    # one (findstr /v /b: tasklist's "no tasks" text, in any language, whose
    # words are never read). findstr exits 1 both for "no match" and for a
    # file it cannot open, so a list that cannot be written or read
    # (tasklist or its redirection failed, findstr cannot open it) writes
    # nothing either (add=unread, exit 2). Native only, cmd.exe, reg.exe,
    # findstr.exe and tasklist.exe: PowerShell took ~8 s to start there
    # (test #2 measures the native chain, tasklist included: the log
    # header's time against REAPER's start). Each run appends to <log dir>\boot-pref.log a header
    # "boot-pref <date> <time> add=<none|held|unread|0|1>" (none: at the
    # original, nothing written and no holder read; cmd's local clock, a
    # diagnostic only: the task's last run (UTC) and last result in
    # Get-IemBootstrapState are authoritative) and reg.exe's query of the
    # value; reg.exe's first query goes to <log dir>\boot-pref.before, which
    # findstr compares with the original's line as reg.exe prints it (a DWORD
    # in lower-case hex). cmd's exit code is 1 when the add failed, 2 when
    # the holders could not be read, else 0. The task runs elevated with the
    # user's environment, so the line holds no %: cmd and Task Scheduler
    # expand %name% before cmd parses the line, while cmd /v:on expands
    # !name! after it, so a value the user set is text, never a command. /d
    # skips cmd's AutoRun, /e:on keeps the extensions whatever the user's
    # HKCU says; every program by its full path; no pipe and no FOR /F (each
    # starts another cmd through COMSPEC). findstr's quote is \^" : the caret
    # makes cmd take the quote as text (its quote state stays as it is, and
    # the findstr command holds no ! for delayed expansion to read the caret
    # again), the backslash is findstr's own escape of a quote. A key, value
    # name, module or path holding a character cmd reads there
    # (" % ! ^ & | < >) or a control character is refused, and a value name
    # holding a backslash (findstr reads it in its literal); the module is a
    # bare .dll name, as Register-IemTasks takes it. -PrefKind: the value's
    # registry kind (dword, text), which Register-IemTasks reads from the
    # value itself.
    param([Parameter(Mandatory)][string]$System, [Parameter(Mandatory)][string]$PrefKey,
          [Parameter(Mandatory)][string]$PrefName, [Parameter(Mandatory)][string]$PrefOriginal,
          [Parameter(Mandatory)][string]$Module, [Parameter(Mandatory)][string]$LogDir,
          [ValidateSet('dword', 'text')][string]$PrefKind = 'dword')
    if ($PrefOriginal -cnotmatch '^[0-9]{1,5}$') { throw "PrefOriginal '$PrefOriginal' refused (the recorded buffer, digits)" }
    if ($Module -cnotmatch $script:ModulePattern) { throw "module name '$Module' refused" }
    $key = ConvertTo-IemRegExeKey -Key $PrefKey
    $System = $System.TrimEnd('\')
    $LogDir = $LogDir.TrimEnd('\')
    foreach ($p in @($System, $LogDir)) {
        if (-not [IO.Path]::IsPathRooted($p)) { throw "$p is not an absolute path (the boot task's command line)" }
    }
    if ($PrefName.Contains('\')) { throw "value name '$PrefName' refused (a backslash: findstr and reg.exe would read it as an escape)" }
    foreach ($v in @($key, $PrefName, $Module, $System, $LogDir)) {
        if ($v -match '[\x00-\x1f"%!^&|<>]') { throw "'$v' refused for the boot task's command line (it holds a character cmd reads)" }
    }
    $reg = '"' + [IO.Path]::Combine($System, 'reg.exe') + '"'
    $findstr = '"' + [IO.Path]::Combine($System, 'findstr.exe') + '"'
    $tasklist = '"' + [IO.Path]::Combine($System, 'tasklist.exe') + '"'
    $log = '"' + [IO.Path]::Combine($LogDir, $script:BootPrefLog) + '"'
    $before = '"' + [IO.Path]::Combine($LogDir, $script:BootPrefBefore) + '"'
    $holders = '"' + [IO.Path]::Combine($LogDir, $script:BootPrefHolders) + '"'
    $type = 'REG_DWORD'
    $shown = '0x{0:x}' -f [int]$PrefOriginal
    if ($PrefKind -eq 'text') {
        $type = 'REG_SZ'
        $shown = $PrefOriginal
    }
    $value = '"{0}" /v "{1}"' -f $key, $PrefName
    $original = '    {0}    {1}    {2}' -f $PrefName, $type, $shown
    $add = '({0} add {1} /t {2} /d {3} /f >nul 2>&1 && set "iemadd=0" || set "iemadd=1")' -f $reg, $value, $type, $PrefOriginal
    # A quote line: held. Else the add only on proof the list was read (a
    # line without the quote: findstr exits 1 for "no match" and for a file
    # it cannot open alike), otherwise unread.
    $held = '({0} /b /l \^" {1} >nul && set "iemadd=held" || ({0} /v /b /l \^" {1} >nul && {2} || set "iemadd=unread"))' -f $findstr, $holders, $add
    $line = ('{0} query {1} >{2} 2>&1' -f $reg, $value, $before) +
        ' & set "iemadd=none"' +
        (' & {0} /i /l /x /c:"{1}" {2} >nul' -f $findstr, $original, $before) +
        (' || ({0} /m "{1}" /fo csv /nh >{2} 2>&1 && {3} || set "iemadd=unread")' -f $tasklist, $Module, $holders, $held) +
        (' & (echo boot-pref !DATE! !TIME! add=!iemadd!)>>{0}' -f $log) +
        (' & {0} query {1} >>{2} 2>&1' -f $reg, $value, $log) +
        ' & if "!iemadd!"=="1" (exit /b 1) else if "!iemadd!"=="unread" (exit /b 2) else (exit /b 0)'
    [pscustomobject]@{ exe = [IO.Path]::Combine($System, 'cmd.exe'); arguments = ('/d /q /e:on /v:on /s /c "' + $line + '"') }
}

Export-ModuleMember -Function *-Iem*
