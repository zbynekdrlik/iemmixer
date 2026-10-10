#Requires -Version 5.1
# IemPc.psm1's scheduled tasks (design note section 5.1; split out of
# IemPc.psm1 by responsibility, #36): our tasks' security descriptor and its
# read-back, the task definitions and their reports, the elevated root's
# path, and Register-IemTasks.
# IemPc.psm1 imports this module from its own folder (-Global, as IemTuning
# imports IemTuningStore, #34), so it goes wherever IemPc.psm1 goes: the
# bundle, the bootstrap's admin-only stage, <elevated root>\tasks and the
# cutover's and the ssh shell undo's folders. Its functions reach the other
# IemPc modules' through the global session state. Nothing is ended by force
# (I8). No site value lives here (P6).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:SidAdmins = 'S-1-5-32-544'
$script:SidSystem = 'S-1-5-18'
# Task Scheduler: TASK_LOGON_INTERACTIVE_TOKEN; TASK_LOGON_S4U (the boot task:
# the user's account without a stored password, before any logon);
# TASK_TRIGGER_BOOT / _LOGON; TASK_RUNLEVEL_LUA / _HIGHEST (below).
$script:LogonInteractive = 3
$script:LogonS4U = 2
$script:TriggerBoot = 8
$script:TriggerLogon = 9
# A driver module's name, as the [card] module names it (no path).
$script:ModulePattern = '^[^\\/:*?"<>|]+\.dll$'
# RegisterTaskDefinition flags: TASK_CREATE_OR_UPDATE (6) and
# TASK_DONT_ADD_PRINCIPAL_ACE (0x10): without it the service adds its own allow
# ACE for the task's user next to ours, and the read-back (exactly our three
# explicit ACEs, design section 5.1) refuses the task. An update keeps the
# task's old descriptor (CI run 36360472125: StartREAPER), so every task then
# gets ours through IRegisteredTask.SetSecurityDescriptor with the same 0x10.
$script:TaskDontAddPrincipalAce = 0x10
$script:TaskCreateOrUpdate = 6 -bor $script:TaskDontAddPrincipalAce
$script:RunLevelLimited = 0
$script:RunLevelHighest = 1
# Access masks as unsigned values: GRGX and FRFX (read and execute), GA and FA (full).
$script:MaskReadExecute = @([int64]2684354560, [int64]1179817)
$script:MaskFull = @([int64]268435456, [int64]2032127)

# ---- scheduled tasks (design section 5.1) ----

function Get-IemTaskSddl {
    # Our tasks' security descriptor: the logged-on user may read and run the task
    # (so the Limited guard can start it); Administrators and SYSTEM own it.
    param([Parameter(Mandatory)][string]$UserSid)
    if ($UserSid -cnotmatch '^S-1-(5-21|12-1)(-[0-9]+){4}$') { throw "not a user SID: $UserSid" }
    return "D:(A;;GRGX;;;$UserSid)(A;;FA;;;BA)(A;;FA;;;SY)"
}

function Test-IemTaskSddl {
    # A task's DACL read back (design section 5.1: the user may read and run the
    # task, never change it; only Administrators and SYSTEM have more). Its
    # explicit ACEs are exactly ours, one allow ACE per SID: the user read and
    # execute, Administrators and SYSTEM full. Task Scheduler may print the
    # rights in generic form (GRGX, GA) or file form (FRFX, FA); both are
    # accepted. ACEs inherited from the task folder (ID) are accepted only for
    # Administrators and SYSTEM, whatever their rights (our explicit ACEs give
    # them full control anyway); an inherited ACE for anyone else (the user,
    # Users, Everyone) refuses. Deny and object ACEs refuse, inherited or not.
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Sddl, [Parameter(Mandatory)][string]$UserSid)
    try { $sd = New-Object System.Security.AccessControl.RawSecurityDescriptor $Sddl } catch { return $false }
    if ($null -eq $sd.DiscretionaryAcl) { return $false }
    $want = @{}
    $want[$UserSid] = $script:MaskReadExecute
    $want[$script:SidAdmins] = $script:MaskFull
    $want[$script:SidSystem] = $script:MaskFull
    $seen = @{}
    foreach ($ace in $sd.DiscretionaryAcl) {
        if ($ace -isnot [System.Security.AccessControl.CommonAce]) { return $false }
        if ($ace.AceQualifier -ne [System.Security.AccessControl.AceQualifier]::AccessAllowed) { return $false }
        $sid = $ace.SecurityIdentifier.Value
        if ($ace.IsInherited) {
            if (@($script:SidAdmins, $script:SidSystem) -cnotcontains $sid) { return $false }
            continue
        }
        if (-not $want.ContainsKey($sid) -or $seen.ContainsKey($sid)) { return $false }
        $mask = ([int64]$ace.AccessMask) -band [int64]4294967295
        if ($want[$sid] -notcontains $mask) { return $false }
        $seen[$sid] = $true
    }
    return ($seen.Count -eq 3)
}

function Connect-IemScheduler {
    $sch = New-Object -ComObject 'Schedule.Service'
    $sch.Connect()
    return $sch
}

function Get-IemTaskFolder {
    # The folder, or $null when it does not exist.
    param([Parameter(Mandatory)]$Scheduler, [Parameter(Mandatory)][string]$Path)
    try { return $Scheduler.GetFolder('\' + $Path.Trim('\')) } catch { return $null }
}

function Get-IemRegisteredTask {
    param([Parameter(Mandatory)]$Scheduler, [Parameter(Mandatory)][string]$Folder, [Parameter(Mandatory)][string]$Name)
    $f = Get-IemTaskFolder -Scheduler $Scheduler -Path $Folder
    if ($null -eq $f) { return $null }
    try { return $f.GetTask($Name) } catch { return $null }
}

function New-IemTaskDefinition {
    # Interactive for the user, no time limit, IgnoreNew, no idle or battery stop,
    # restart on failure 3 x 1 min (spec section 2.1), normal priority (the default 7
    # would pass below-normal on to the guard's children), never ended hard.
    # -AtBoot (the boot task, #35): S4U (the user's account before any logon,
    # no stored password), the system's start its only trigger, and no other
    # start: no restart on failure, no late start, no start on demand (a later
    # run could meet a process that holds the driver).
    param([Parameter(Mandatory)]$Scheduler, [Parameter(Mandatory)][string]$User, [Parameter(Mandatory)][int]$RunLevel,
          [Parameter(Mandatory)][string]$Exe, [string]$Arguments = '', [string]$WorkDir = '', [string]$Description = '',
          [switch]$AtLogon, [switch]$AtBoot)
    if ($AtLogon -and $AtBoot) { throw 'a task starts at the logon or at the system start, not both' }
    $d = $Scheduler.NewTask(0)
    $d.RegistrationInfo.Description = $Description
    $d.Principal.UserId = $User
    $d.Principal.LogonType = $script:LogonInteractive
    if ($AtBoot) { $d.Principal.LogonType = $script:LogonS4U }
    $d.Principal.RunLevel = $RunLevel
    $s = $d.Settings
    $s.Enabled = $true
    $s.Hidden = $false
    $s.ExecutionTimeLimit = 'PT0S'
    $s.MultipleInstances = 2
    $s.DisallowStartIfOnBatteries = $false
    $s.StopIfGoingOnBatteries = $false
    $s.RunOnlyIfIdle = $false
    $s.IdleSettings.StopOnIdleEnd = $false
    $s.AllowHardTerminate = $false
    $s.StartWhenAvailable = $false
    if ($AtBoot) {
        $s.AllowDemandStart = $false
    } else {
        $s.RestartCount = 3
        $s.RestartInterval = 'PT1M'
    }
    $s.Priority = 4
    $a = $d.Actions.Create(0)
    $a.Path = $Exe
    if ($Arguments) { $a.Arguments = $Arguments }
    if ($WorkDir) { $a.WorkingDirectory = $WorkDir }
    if ($AtLogon) {
        $t = $d.Triggers.Create($script:TriggerLogon)
        $t.UserId = $User
        $t.Enabled = $true
    }
    if ($AtBoot) {
        $t = $d.Triggers.Create($script:TriggerBoot)
        $t.Enabled = $true
    }
    return $d
}

function Get-IemTaskActions {
    param([Parameter(Mandatory)]$Definition)
    $out = @()
    foreach ($a in $Definition.Actions) {
        if ([int]$a.Type -ne 0) {
            $out += [pscustomobject]@{ path = ('action type {0}' -f $a.Type); arguments = ''; workdir = '' }
            continue
        }
        $out += [pscustomobject]@{ path = [string]$a.Path; arguments = [string]$a.Arguments; workdir = [string]$a.WorkingDirectory }
    }
    return ,$out
}

function Test-IemSameActions {
    param([object[]]$A = @(), [object[]]$B = @())
    $x = @($A); $y = @($B)
    if ($x.Count -ne $y.Count) { return $false }
    for ($i = 0; $i -lt $x.Count; $i++) {
        foreach ($k in @('path', 'arguments', 'workdir')) {
            if ([string](Get-IemProp $x[$i] $k) -cne [string](Get-IemProp $y[$i] $k)) { return $false }
        }
    }
    return $true
}

function Get-IemTaskReport {
    # A registered task read back through Task Scheduler.
    param([Parameter(Mandatory)]$Task, [Parameter(Mandatory)][string]$UserSid)
    $d = $Task.Definition
    $s = $d.Settings
    $triggers = @()
    $repeat = @()
    $delays = @()
    $disabled = 0
    foreach ($t in $d.Triggers) {
        $triggers += [int]$t.Type
        $iv = [string]$t.Repetition.Interval
        if ($iv) { $repeat += $iv }
        # Delay exists on the boot and logon triggers (not on a time trigger).
        if ($null -ne $t.PSObject.Properties['Delay']) {
            $dl = [string]$t.Delay
            if ($dl) { $delays += $dl }
        }
        if (-not [bool]$t.Enabled) { $disabled++ }
    }
    $sddl = [string]$Task.GetSecurityDescriptor(4)
    [pscustomobject]@{
        task = [string]$Task.Name
        path = [string]$Task.Path
        run_level = [int]$d.Principal.RunLevel
        logon_type = [int]$d.Principal.LogonType
        time_limit = [string]$s.ExecutionTimeLimit
        instances = [int]$s.MultipleInstances
        restart = ('{0}x{1}' -f $s.RestartCount, $s.RestartInterval)
        demand_start = [bool]$s.AllowDemandStart
        late_start = [bool]$s.StartWhenAvailable
        repeat = ($repeat -join ',')
        delays = ($delays -join ',')
        disabled = $disabled
        batteries = ((-not $s.DisallowStartIfOnBatteries) -and (-not $s.StopIfGoingOnBatteries))
        idle_stop = [bool]$s.IdleSettings.StopOnIdleEnd
        hard_end = [bool]$s.AllowHardTerminate
        priority = [int]$s.Priority
        actions = (Get-IemTaskActions -Definition $d)
        triggers = $triggers
        sddl = $sddl
        sddl_ok = (Test-IemTaskSddl -Sddl $sddl -UserSid $UserSid)
    }
}

function Test-IemTaskReport {
    # What every task of ours must read back as; returns the differences.
    # -Boot (the boot task, #35): S4U, no restart and no start on demand. No
    # task of ours starts late, repeats, waits a trigger delay (it would push
    # the boot task's write toward REAPER's start) or has a disabled trigger.
    param([Parameter(Mandatory)]$Report, [Parameter(Mandatory)][int]$RunLevel, [switch]$Boot)
    $logon = $script:LogonInteractive
    $restart = '3xPT1M'
    $demand = $true
    if ($Boot) {
        $logon = $script:LogonS4U
        $restart = '0x'
        $demand = $false
    }
    $bad = @()
    if ($Report.logon_type -ne $logon) { $bad += "logon type $($Report.logon_type)" }
    if ($Report.run_level -ne $RunLevel) { $bad += "run level $($Report.run_level)" }
    if ($Report.time_limit -cne 'PT0S') { $bad += "time limit $($Report.time_limit)" }
    if ($Report.instances -ne 2) { $bad += "instances $($Report.instances)" }
    if ($Report.restart -cne $restart) { $bad += "restart $($Report.restart)" }
    if ($Report.demand_start -ne $demand) { $bad += "start on demand $($Report.demand_start)" }
    if ($Report.late_start) { $bad += 'starts late after a missed start' }
    if ($Report.repeat) { $bad += "repeats $($Report.repeat)" }
    if ($Report.delays) { $bad += "trigger delay $($Report.delays)" }
    if ($Report.disabled -gt 0) { $bad += "$($Report.disabled) disabled trigger(s)" }
    if (-not $Report.batteries) { $bad += 'stops on batteries' }
    if ($Report.idle_stop) { $bad += 'stops on idle end' }
    if ($Report.hard_end) { $bad += 'may be ended hard' }
    if ($Report.priority -ne 4) { $bad += "priority $($Report.priority)" }
    if (-not $Report.sddl_ok) { $bad += "security descriptor $($Report.sddl)" }
    return ,$bad
}

function Resolve-IemElevatedRoot {
    # The elevated root: the given absolute path, or %ProgramData%\iemmixer
    # from the known folder (never from an environment variable).
    param([string]$ElevatedRoot = '')
    if (-not $ElevatedRoot) {
        $pd = [Environment]::GetFolderPath('CommonApplicationData')
        if (-not $pd) { throw 'the ProgramData known folder is unknown' }
        $ElevatedRoot = Join-Path $pd 'iemmixer'
    }
    $ElevatedRoot = $ElevatedRoot.TrimEnd('\')
    if (-not [IO.Path]::IsPathRooted($ElevatedRoot)) { throw "the elevated root $ElevatedRoot is not an absolute path" }
    return $ElevatedRoot
}

function Register-IemTasks {
    # Our tasks under -Folder (design section 5.1): the guard, StartApp (its exe
    # directly, never its launcher script), the probe and the four Highest
    # tasks (tuning, exclude, logon, boot-pref), each with the security
    # descriptor that lets the Limited guard run it (the boot task never runs
    # on demand, #35: Get-IemBootPrefCommand; it writes the kind the value has
    # in this session's HKCU, Get-IemPrefKind, so run this as the desktop
    # user, as `iempc bootstrap` does). StartREAPER predates S6 and holds site
    # values: it keeps its definition, gains only the descriptor, and must
    # exist (nothing is registered without it). The Highest tasks run this
    # module from a copy in -ElevatedRoot\tasks and load S1c's tuning module
    # from -ElevatedRoot\tuning (default %ProgramData%\iemmixer, resolved here
    # from the known folder and passed on the command line, so the elevated
    # process never reads an environment variable for it); only Administrators
    # and SYSTEM may change that root. Each task's descriptor is set explicitly
    # (SetSecurityDescriptor) before its read-back, because an update keeps a
    # task's old one; any difference throws after every task was tried. Every
    # row names its warnings: Fast Startup that is really on (or that could
    # not be judged), on the boot task's row (Get-IemBootTaskWarnings, also
    # written as a warning), never a refusal. -FastStartup: a judged Fast
    # Startup (Get-IemFastStartup's shape) in place of this PC's facts
    # (Get-IemFastStartupState, the default).
    param(
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][string]$AppExe,
        [Parameter(Mandatory)][string]$PrefKey,
        [Parameter(Mandatory)][string]$PrefName,
        [Parameter(Mandatory)][string]$PrefOriginal,
        [Parameter(Mandatory)][string]$Module,
        [string]$Folder = '\iemmixer',
        [string]$User = '',
        [string]$ElevatedRoot = '',
        $FastStartup = $null
    )
    $ElevatedRoot = Resolve-IemElevatedRoot -ElevatedRoot $ElevatedRoot
    $userRoot = $Root.TrimEnd('\') + '\'
    if (($ElevatedRoot + '\').StartsWith($userRoot, [StringComparison]::OrdinalIgnoreCase) -or
        $userRoot.StartsWith($ElevatedRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw "the elevated root $ElevatedRoot and the user's root $Root must not contain each other"
    }
    if ($PrefOriginal -cnotmatch '^[0-9]{1,5}$') { throw "PrefOriginal '$PrefOriginal' refused (the recorded buffer, digits)" }
    # The driver module ([card] module): the logon task and the boot task
    # never write the preference while a process holds it (#9 2026-09-28;
    # #35, review of PR #40).
    if ($Module -cnotmatch $script:ModulePattern) { throw "module name '$Module' refused" }
    foreach ($v in @($Root, $AppExe, $PrefKey, $PrefName, $ElevatedRoot, $Module)) { [void](Format-IemArg -Value $v) }
    if (-not (Test-Path -LiteralPath $AppExe -PathType Leaf)) { throw "the app exe $AppExe does not exist" }
    $tasksDir = Join-Path $ElevatedRoot 'tasks'
    $system = [Environment]::GetFolderPath('System')
    # The boot task's command line (#35): what cmd would read is refused here,
    # then the value's kind is read (a value that cannot be read is refused),
    # all before anything is written.
    $outDir = Join-Path $tasksDir 'out'
    [void](Get-IemBootPrefCommand -System $system -PrefKey $PrefKey -PrefName $PrefName -PrefOriginal $PrefOriginal -Module $Module -LogDir $outDir)
    $prefKind = Get-IemPrefKind -Key $PrefKey -Name $PrefName
    $boot = Get-IemBootPrefCommand -System $system -PrefKey $PrefKey -PrefName $PrefName -PrefOriginal $PrefOriginal `
        -Module $Module -PrefKind $prefKind -LogDir $outDir
    $u = Resolve-IemUser -User $User
    $sddl = Get-IemTaskSddl -UserSid $u.sid
    $sch = Connect-IemScheduler
    $reaper = Get-IemRegisteredTask -Scheduler $sch -Folder $Folder -Name 'iemmixer-StartREAPER'
    if ($null -eq $reaper) {
        throw "$Folder\iemmixer-StartREAPER is missing: it predates S6 (S1a runbook) and holds site values; create it first"
    }
    $reaperDef = $reaper.Definition
    if ([int]$reaperDef.Principal.LogonType -ne $script:LogonInteractive) {
        throw "iemmixer-StartREAPER logs on as type $($reaperDef.Principal.LogonType), not Interactive: re-create it (S1a runbook)"
    }
    $reaperActions = Get-IemTaskActions -Definition $reaperDef
    $f = Get-IemTaskFolder -Scheduler $sch -Path $Folder
    Install-IemElevatedDir -Root $ElevatedRoot -UserSid $u.sid

    # After the cutover (S8, IemCutover's Set-IemGuardLogon) the guard task
    # starts at the logon: registering again keeps that trigger.
    $guardTask = Get-IemRegisteredTask -Scheduler $sch -Folder $Folder -Name 'iemmixer-guard'
    $guardTrigger = ''
    if ($null -ne $guardTask -and @((Get-IemTaskReport -Task $guardTask -UserSid $u.sid).triggers) -contains $script:TriggerLogon) { $guardTrigger = 'logon' }
    $ps = Join-Path $system 'WindowsPowerShell\v1.0\powershell.exe'
    $common = '-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File ' +
        (Format-IemArg (Join-Path $tasksDir 'iem-task.ps1')) + ' -Root ' + (Format-IemArg $Root) +
        ' -TuningDir ' + (Format-IemArg (Join-Path $ElevatedRoot 'tuning'))
    $logonArgs = $common + ' -Kind logon -PrefKey ' + (Format-IemArg $PrefKey) + ' -PrefName ' + (Format-IemArg $PrefName) +
        ' -PrefOriginal ' + (Format-IemArg $PrefOriginal) + ' -Module ' + (Format-IemArg $Module)
    $specs = @(
        @{ name = 'iemmixer-guard'; level = $script:RunLevelLimited; exe = (Join-Path $Root 'bin\iemmixer-guard.exe'); args = 'run'; dir = $Root; trigger = $guardTrigger },
        @{ name = 'iemmixer-StartApp'; level = $script:RunLevelLimited; exe = $AppExe; args = ''; dir = (Split-Path -Parent $AppExe); trigger = '' },
        @{ name = 'iemmixer-probe'; level = $script:RunLevelLimited; exe = (Join-Path $system 'cmd.exe'); args = '/c exit 0'; dir = ''; trigger = '' },
        @{ name = 'iemmixer-tuning'; level = $script:RunLevelHighest; exe = $ps; args = ($common + ' -Kind tuning'); dir = $tasksDir; trigger = '' },
        @{ name = 'iemmixer-exclude'; level = $script:RunLevelHighest; exe = $ps; args = ($common + ' -Kind exclude'); dir = $tasksDir; trigger = '' },
        @{ name = 'iemmixer-logon'; level = $script:RunLevelHighest; exe = $ps; args = $logonArgs; dir = $tasksDir; trigger = 'logon' },
        @{ name = 'iemmixer-boot-pref'; level = $script:RunLevelHighest; exe = $boot.exe; args = $boot.arguments; dir = $tasksDir; trigger = 'boot' }
    )
    # Fast Startup that is really on skips the boot task's trigger: named (a
    # warning and the boot task's row), never a refusal (#35, review of PR #40).
    if ($null -eq $FastStartup) { $FastStartup = Get-IemFastStartupState }
    $bootWarnings = Get-IemBootTaskWarnings -FastStartup $FastStartup
    foreach ($w in $bootWarnings) { Write-Warning $w }
    $reports = @()
    $problems = @()
    foreach ($s in $specs) {
        $atLogon = ($s.trigger -ceq 'logon')
        $atBoot = ($s.trigger -ceq 'boot')
        $warnings = @()
        if ($atBoot) { $warnings = $bootWarnings }
        $d = New-IemTaskDefinition -Scheduler $sch -User $u.name -RunLevel $s.level -Exe $s.exe -Arguments $s.args `
            -WorkDir $s.dir -Description ('iemmixer S6: ' + $s.name) -AtLogon:$atLogon -AtBoot:$atBoot
        [void]$f.RegisterTaskDefinition($s.name, $d, $script:TaskCreateOrUpdate, $u.name, $null, [int]$d.Principal.LogonType, $sddl)
        [void]$f.GetTask($s.name).SetSecurityDescriptor($sddl, $script:TaskDontAddPrincipalAce)
        $rep = Get-IemTaskReport -Task $f.GetTask($s.name) -UserSid $u.sid
        $bad = Test-IemTaskReport -Report $rep -RunLevel $s.level -Boot:$atBoot
        $want = [pscustomobject]@{ path = $s.exe; arguments = $s.args; workdir = $s.dir }
        if (-not (Test-IemSameActions -A @($want) -B $rep.actions)) { $bad += ('action ' + (ConvertTo-Json -InputObject $rep.actions -Compress)) }
        $wantTriggers = ''
        if ($atLogon) { $wantTriggers = [string]$script:TriggerLogon }
        if ($atBoot) { $wantTriggers = [string]$script:TriggerBoot }
        if ((@($rep.triggers) -join ',') -cne $wantTriggers) { $bad += ('triggers ' + (@($rep.triggers) -join ',')) }
        foreach ($b in $bad) { $problems += ('{0}: {1}' -f $s.name, $b) }
        $reports += [pscustomobject]@{ task = $s.name; run_level = $rep.run_level; sddl_ok = $rep.sddl_ok; actions = $rep.actions; triggers = $rep.triggers; problems = $bad
                                       warnings = $warnings }
    }

    # StartREAPER: its definition untouched (never registered again), only our
    # descriptor (a TASK_UPDATE registration kept its old one).
    [void]$f.GetTask('iemmixer-StartREAPER').SetSecurityDescriptor($sddl, $script:TaskDontAddPrincipalAce)
    $rep = Get-IemTaskReport -Task $f.GetTask('iemmixer-StartREAPER') -UserSid $u.sid
    $bad = @()
    if (-not $rep.sddl_ok) { $bad += ('security descriptor ' + $rep.sddl) }
    if (-not (Test-IemSameActions -A $reaperActions -B $rep.actions)) { $bad += 'its action changed' }
    foreach ($b in $bad) { $problems += ('iemmixer-StartREAPER: {0}' -f $b) }
    $reports += [pscustomobject]@{ task = 'iemmixer-StartREAPER'; run_level = $rep.run_level; sddl_ok = $rep.sddl_ok; actions = $rep.actions; triggers = $rep.triggers; problems = $bad
                                   warnings = @() }

    if ($problems.Count -gt 0) { throw ('task read-back: ' + ($problems -join '; ')) }
    return ,$reports
}

Export-ModuleMember -Function *-Iem*
