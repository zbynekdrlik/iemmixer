#Requires -Version 5.1
# S6 PC module (design note section 5.1, section 6, section 7), shipped in every bundle:
# - bootstrap, run elevated over ssh (`iempc bootstrap <function>`); every
#   function is idempotent and reads back what it wrote;
# - the body of the elevated tasks (Invoke-IemTaskRequest);
# - the helpers of hil-v1.ps1.
# Nothing is ended by force (I8). No site value lives here (P6): paths, the
# driver module, the preference key, the app and the service come as
# parameters from the private env or the site.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:ModuleFile = $PSCommandPath
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding $false
# Functions that `return ,$array` hand back exactly one array: assign their
# result (`$x = Test-IemBundleSums ...`); `@(Test-IemBundleSums ...)` would nest it.

$script:TaskNames = @('iemmixer-guard', 'iemmixer-StartREAPER', 'iemmixer-StartApp', 'iemmixer-probe',
                      'iemmixer-tuning', 'iemmixer-exclude', 'iemmixer-logon')
$script:SidAdmins = 'S-1-5-32-544'
$script:SidSystem = 'S-1-5-18'
# Task Scheduler: TASK_LOGON_INTERACTIVE_TOKEN, TASK_RUNLEVEL_LUA / _HIGHEST.
$script:LogonInteractive = 3
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
# Service rights the user needs on the tunnel's service: RP start, WP stop, LO query.
$script:ServiceRights = 176
# The pinned runner (actions/runner release, win-x64).
$script:RunnerVersion = '2.337.0'
$script:RunnerSha256 = '1150692afa94e71f872017e254ea55b6eece1eece3fe7e3a6d4c93d0a1b85cfc'
$script:OpsUrl = 'https://github.com/zbynekdrlik/iemmixer-ops'
# The four verbs the tuning task accepts (design section 5.1).
$script:TuningVerbs = @('enter', 'exit', 'state', 'apply-tier2')

# The elevated tasks' entry script, written by Register-IemTasks next to its
# copy of this module.
$script:TaskEntry = @'
# iemmixer S6: the elevated tasks' entry (tuning, exclude, logon), written by
# Register-IemTasks next to its copy of IemPc.psm1 in <elevated root>\tasks
# (owner Administrators; only Administrators and SYSTEM may change it). It
# reads <Root>\guard\tasks\<kind>.request.json from the user's root and writes
# <elevated root>\tasks\out\<kind>.result.json, which the user may only read.
param([Parameter(Mandatory)][string]$Kind, [Parameter(Mandatory)][string]$Root,
      [Parameter(Mandatory)][string]$TuningDir,
      [string]$PrefKey = '', [string]$PrefName = '', [string]$PrefOriginal = '')
# This process runs elevated: modules load only from Windows PowerShell's own
# folders, never from the user's Documents or an HKCU environment's path.
$pinned = [IO.Path]::Combine($PSHOME, 'Modules') + ';' + [IO.Path]::Combine([Environment]::GetFolderPath('ProgramFiles'), 'WindowsPowerShell\Modules')
$env:PSModulePath = $pinned
$ErrorActionPreference = 'Stop'
try {
    Import-Module ([IO.Path]::Combine($PSScriptRoot, 'IemPc.psm1')) -Force
    $out = [IO.Path]::Combine($PSScriptRoot, 'out')
    $r = Invoke-IemTaskRequest -Kind $Kind -Root $Root -OutDir $out -TuningDir $TuningDir -PrefKey $PrefKey -PrefName $PrefName -PrefOriginal $PrefOriginal
    if ($r.ok) { exit 0 }
    exit 1
} catch {
    [Console]::Error.WriteLine('iem-task: ' + $_.Exception.Message)
    exit 2
}
'@

# ---- small helpers ----

function Get-IemProp {
    # A property of a parsed JSON object, or $null (Set-StrictMode refuses a missing one).
    param($Object, [Parameter(Mandatory)][string]$Name)
    if ($null -eq $Object) { return $null }
    $p = $Object.PSObject.Properties[$Name]
    if ($null -eq $p) { return $null }
    return $p.Value
}

function Write-IemJsonFile {
    # JSON, UTF-8 without a BOM, written to a temp file and moved over the target.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)]$Value)
    $tmp = $Path + '.tmp'
    [IO.File]::WriteAllText($tmp, (ConvertTo-Json -InputObject $Value -Depth 8), $script:Utf8NoBom)
    Move-Item -LiteralPath $tmp -Destination $Path -Force
}

function Format-IemArg {
    # One command-line argument in double quotes. A value never holds a quote; a
    # trailing backslash would escape the closing one, so it is dropped.
    param([Parameter(Mandatory)][string]$Value)
    if ($Value.Contains('"')) { throw "argument refused (it holds a double quote): $Value" }
    return '"' + $Value.TrimEnd('\') + '"'
}

function Resolve-IemUser {
    # The account our tasks run as: the given name, or this session's token (over
    # ssh USERDOMAIN is the workgroup, so the token's own name is used).
    param([string]$User = '')
    if (-not $User) { $User = [Security.Principal.WindowsIdentity]::GetCurrent().Name }
    $sid = (New-Object Security.Principal.NTAccount $User).Translate([Security.Principal.SecurityIdentifier]).Value
    [pscustomobject]@{ name = $User; sid = $sid }
}

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
    param([Parameter(Mandatory)]$Scheduler, [Parameter(Mandatory)][string]$User, [Parameter(Mandatory)][int]$RunLevel,
          [Parameter(Mandatory)][string]$Exe, [string]$Arguments = '', [string]$WorkDir = '', [string]$Description = '',
          [switch]$AtLogon)
    $d = $Scheduler.NewTask(0)
    $d.RegistrationInfo.Description = $Description
    $d.Principal.UserId = $User
    $d.Principal.LogonType = $script:LogonInteractive
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
    $s.RestartCount = 3
    $s.RestartInterval = 'PT1M'
    $s.Priority = 4
    $a = $d.Actions.Create(0)
    $a.Path = $Exe
    if ($Arguments) { $a.Arguments = $Arguments }
    if ($WorkDir) { $a.WorkingDirectory = $WorkDir }
    if ($AtLogon) {
        $t = $d.Triggers.Create(9)
        $t.UserId = $User
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
    foreach ($t in $d.Triggers) { $triggers += [int]$t.Type }
    $sddl = [string]$Task.GetSecurityDescriptor(4)
    [pscustomobject]@{
        task = [string]$Task.Name
        path = [string]$Task.Path
        run_level = [int]$d.Principal.RunLevel
        logon_type = [int]$d.Principal.LogonType
        time_limit = [string]$s.ExecutionTimeLimit
        instances = [int]$s.MultipleInstances
        restart = ('{0}x{1}' -f $s.RestartCount, $s.RestartInterval)
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
    param([Parameter(Mandatory)]$Report, [Parameter(Mandatory)][int]$RunLevel)
    $bad = @()
    if ($Report.logon_type -ne $script:LogonInteractive) { $bad += "logon type $($Report.logon_type)" }
    if ($Report.run_level -ne $RunLevel) { $bad += "run level $($Report.run_level)" }
    if ($Report.time_limit -cne 'PT0S') { $bad += "time limit $($Report.time_limit)" }
    if ($Report.instances -ne 2) { $bad += "instances $($Report.instances)" }
    if ($Report.restart -cne '3xPT1M') { $bad += "restart $($Report.restart)" }
    if (-not $Report.batteries) { $bad += 'stops on batteries' }
    if ($Report.idle_stop) { $bad += 'stops on idle end' }
    if ($Report.hard_end) { $bad += 'may be ended hard' }
    if ($Report.priority -ne 4) { $bad += "priority $($Report.priority)" }
    if (-not $Report.sddl_ok) { $bad += "security descriptor $($Report.sddl)" }
    return ,$bad
}

function Register-IemTasks {
    # Our tasks under -Folder (design section 5.1): the guard, StartApp (its exe
    # directly, never its launcher script), the probe and the three Highest
    # tasks (tuning, exclude, logon), each with the security descriptor that
    # lets the Limited guard run it. StartREAPER predates S6 and holds site
    # values: it keeps its definition, gains only the descriptor, and must
    # exist (nothing is registered without it). The Highest tasks run this
    # module from a copy in -ElevatedRoot\tasks and load S1c's tuning module
    # from -ElevatedRoot\tuning (default %ProgramData%\iemmixer, resolved here
    # from the known folder and passed on the command line, so the elevated
    # process never reads an environment variable for it); only Administrators
    # and SYSTEM may change that root. Each task's descriptor is set explicitly
    # (SetSecurityDescriptor) before its read-back, because an update keeps a
    # task's old one; any difference throws after every task was tried.
    param(
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][string]$AppExe,
        [Parameter(Mandatory)][string]$PrefKey,
        [Parameter(Mandatory)][string]$PrefName,
        [Parameter(Mandatory)][string]$PrefOriginal,
        [string]$Folder = '\iemmixer',
        [string]$User = '',
        [string]$ElevatedRoot = ''
    )
    if (-not $ElevatedRoot) {
        $pd = [Environment]::GetFolderPath('CommonApplicationData')
        if (-not $pd) { throw 'the ProgramData known folder is unknown' }
        $ElevatedRoot = Join-Path $pd 'iemmixer'
    }
    $ElevatedRoot = $ElevatedRoot.TrimEnd('\')
    if (-not [IO.Path]::IsPathRooted($ElevatedRoot)) { throw "the elevated root $ElevatedRoot is not an absolute path" }
    $userRoot = $Root.TrimEnd('\') + '\'
    if (($ElevatedRoot + '\').StartsWith($userRoot, [StringComparison]::OrdinalIgnoreCase) -or
        $userRoot.StartsWith($ElevatedRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw "the elevated root $ElevatedRoot and the user's root $Root must not contain each other"
    }
    if ($PrefOriginal -cnotmatch '^[0-9]{1,5}$') { throw "PrefOriginal '$PrefOriginal' refused (the recorded buffer, digits)" }
    foreach ($v in @($Root, $AppExe, $PrefKey, $PrefName, $ElevatedRoot)) { [void](Format-IemArg -Value $v) }
    if (-not (Test-Path -LiteralPath $AppExe -PathType Leaf)) { throw "the app exe $AppExe does not exist" }
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
    $tasksDir = Join-Path $ElevatedRoot 'tasks'

    $system = [Environment]::GetFolderPath('System')
    $ps = Join-Path $system 'WindowsPowerShell\v1.0\powershell.exe'
    $common = '-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File ' +
        (Format-IemArg (Join-Path $tasksDir 'iem-task.ps1')) + ' -Root ' + (Format-IemArg $Root) +
        ' -TuningDir ' + (Format-IemArg (Join-Path $ElevatedRoot 'tuning'))
    $logonArgs = $common + ' -Kind logon -PrefKey ' + (Format-IemArg $PrefKey) + ' -PrefName ' + (Format-IemArg $PrefName) +
        ' -PrefOriginal ' + (Format-IemArg $PrefOriginal)
    $specs = @(
        @{ name = 'iemmixer-guard'; level = $script:RunLevelLimited; exe = (Join-Path $Root 'bin\iemmixer-guard.exe'); args = 'run'; dir = $Root; logon = $false },
        @{ name = 'iemmixer-StartApp'; level = $script:RunLevelLimited; exe = $AppExe; args = ''; dir = (Split-Path -Parent $AppExe); logon = $false },
        @{ name = 'iemmixer-probe'; level = $script:RunLevelLimited; exe = (Join-Path $system 'cmd.exe'); args = '/c exit 0'; dir = ''; logon = $false },
        @{ name = 'iemmixer-tuning'; level = $script:RunLevelHighest; exe = $ps; args = ($common + ' -Kind tuning'); dir = $tasksDir; logon = $false },
        @{ name = 'iemmixer-exclude'; level = $script:RunLevelHighest; exe = $ps; args = ($common + ' -Kind exclude'); dir = $tasksDir; logon = $false },
        @{ name = 'iemmixer-logon'; level = $script:RunLevelHighest; exe = $ps; args = $logonArgs; dir = $tasksDir; logon = $true }
    )
    $reports = @()
    $problems = @()
    foreach ($s in $specs) {
        $d = New-IemTaskDefinition -Scheduler $sch -User $u.name -RunLevel $s.level -Exe $s.exe -Arguments $s.args `
            -WorkDir $s.dir -Description ('iemmixer S6: ' + $s.name) -AtLogon:$s.logon
        [void]$f.RegisterTaskDefinition($s.name, $d, $script:TaskCreateOrUpdate, $u.name, $null, $script:LogonInteractive, $sddl)
        [void]$f.GetTask($s.name).SetSecurityDescriptor($sddl, $script:TaskDontAddPrincipalAce)
        $rep = Get-IemTaskReport -Task $f.GetTask($s.name) -UserSid $u.sid
        $bad = Test-IemTaskReport -Report $rep -RunLevel $s.level
        $want = [pscustomobject]@{ path = $s.exe; arguments = $s.args; workdir = $s.dir }
        if (-not (Test-IemSameActions -A @($want) -B $rep.actions)) { $bad += ('action ' + (ConvertTo-Json -InputObject $rep.actions -Compress)) }
        $wantTriggers = ''
        if ($s.logon) { $wantTriggers = '9' }
        if ((@($rep.triggers) -join ',') -cne $wantTriggers) { $bad += ('triggers ' + (@($rep.triggers) -join ',')) }
        foreach ($b in $bad) { $problems += ('{0}: {1}' -f $s.name, $b) }
        $reports += [pscustomobject]@{ task = $s.name; run_level = $rep.run_level; sddl_ok = $rep.sddl_ok; actions = $rep.actions; triggers = $rep.triggers; problems = $bad }
    }

    # StartREAPER: its definition untouched (never registered again), only our
    # descriptor (a TASK_UPDATE registration kept its old one).
    [void]$f.GetTask('iemmixer-StartREAPER').SetSecurityDescriptor($sddl, $script:TaskDontAddPrincipalAce)
    $rep = Get-IemTaskReport -Task $f.GetTask('iemmixer-StartREAPER') -UserSid $u.sid
    $bad = @()
    if (-not $rep.sddl_ok) { $bad += ('security descriptor ' + $rep.sddl) }
    if (-not (Test-IemSameActions -A $reaperActions -B $rep.actions)) { $bad += 'its action changed' }
    foreach ($b in $bad) { $problems += ('iemmixer-StartREAPER: {0}' -f $b) }
    $reports += [pscustomobject]@{ task = 'iemmixer-StartREAPER'; run_level = $rep.run_level; sddl_ok = $rep.sddl_ok; actions = $rep.actions; triggers = $rep.triggers; problems = $bad }

    if ($problems.Count -gt 0) { throw ('task read-back: ' + ($problems -join '; ')) }
    return ,$reports
}

# ---- folders and their DACLs (design section 6) ----

function Get-IemRootRights {
    # The PC root: the user, SYSTEM and Administrators, full control.
    param([Parameter(Mandatory)][string]$UserSid)
    $full = [System.Security.AccessControl.FileSystemRights]::FullControl
    $r = @{}
    $r[$UserSid] = $full
    $r[$script:SidSystem] = $full
    $r[$script:SidAdmins] = $full
    return $r
}

function Get-IemElevatedRights {
    # The Highest tasks' folder: Administrators and SYSTEM change it, the user only reads.
    param([Parameter(Mandatory)][string]$UserSid)
    $r = @{}
    $r[$script:SidAdmins] = [System.Security.AccessControl.FileSystemRights]::FullControl
    $r[$script:SidSystem] = [System.Security.AccessControl.FileSystemRights]::FullControl
    $r[$UserSid] = [System.Security.AccessControl.FileSystemRights]::ReadAndExecute
    return $r
}

function Set-IemDirectoryAcl {
    # A protected DACL (nothing inherited from above) that everything below inherits.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][hashtable]$Rights)
    $acl = New-Object System.Security.AccessControl.DirectorySecurity
    $acl.SetAccessRuleProtection($true, $false)
    foreach ($sid in $Rights.Keys) {
        $id = New-Object System.Security.Principal.SecurityIdentifier $sid
        $rule = New-Object System.Security.AccessControl.FileSystemAccessRule($id, $Rights[$sid], 'ContainerInherit, ObjectInherit', 'None', 'Allow')
        $acl.AddAccessRule($rule)
    }
    [System.IO.Directory]::SetAccessControl($Path, $acl)
}

function Test-IemDirectoryAcl {
    # The DACL read back against Set-IemDirectoryAcl; returns the differences.
    # Exact: that DACL is protected, so it inherits nothing, and an inherited
    # rule (whoever it names) is a difference like any other.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][hashtable]$Rights)
    $acl = Get-Acl -LiteralPath $Path
    $bad = @()
    if (-not $acl.AreAccessRulesProtected) { $bad += 'inherits from its parent' }
    $seen = @{}
    foreach ($r in @($acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))) {
        $sid = $r.IdentityReference.Value
        if (-not $Rights.ContainsKey($sid)) { $bad += "a rule for $sid"; continue }
        if ("$($r.AccessControlType)" -ne 'Allow') { $bad += "a deny rule for $sid"; continue }
        if ($r.IsInherited) { $bad += "an inherited rule for $sid" }
        # An allow rule always carries Synchronize (1048576).
        $want = ([int]$Rights[$sid]) -bor 1048576
        if ([int]$r.FileSystemRights -ne $want -or [int]$r.InheritanceFlags -ne 3 -or [int]$r.PropagationFlags -ne 0) {
            $bad += ('{0}: {1} ({2}, {3})' -f $sid, $r.FileSystemRights, $r.InheritanceFlags, $r.PropagationFlags)
        }
        if ($seen.ContainsKey($sid)) { $bad += "two rules for $sid" }
        $seen[$sid] = $true
    }
    foreach ($sid in $Rights.Keys) { if (-not $seen.ContainsKey($sid)) { $bad += "no rule for $sid" } }
    return ,$bad
}

function Set-IemRootAcl {
    # The PC root (%LOCALAPPDATA%\iemmixer, design section 6): a protected DACL for the
    # user, SYSTEM and Administrators that everything below inherits, so the
    # staging copies of `iem-migrate band` need no ACL work (#20).
    param([Parameter(Mandatory)][string]$Root, [string]$User = '')
    $u = Resolve-IemUser -User $User
    if (-not (Test-Path -LiteralPath $Root -PathType Container)) { New-Item -ItemType Directory -Force -Path $Root | Out-Null }
    $rights = Get-IemRootRights -UserSid $u.sid
    $before = Test-IemDirectoryAcl -Path $Root -Rights $rights
    if ($before.Count -gt 0) { Set-IemDirectoryAcl -Path $Root -Rights $rights }
    $after = Test-IemDirectoryAcl -Path $Root -Rights $rights
    if ($after.Count -gt 0) { throw ('root ACL read-back: ' + ($after -join '; ')) }
    [pscustomobject]@{ root = $Root; user = $u.name; changed = ($before.Count -gt 0); before = $before }
}

function Test-IemReparsePoint {
    # Whether a file or folder is a junction or a link (the attributes of the
    # link itself, so a dangling one counts too). The elevated code never
    # follows one. $false when nothing is there.
    param([Parameter(Mandatory)][string]$Path)
    try { $a = [IO.File]::GetAttributes($Path) } catch {
        $e = $_.Exception
        while ($null -ne $e.InnerException) { $e = $e.InnerException }
        if ($e -is [IO.FileNotFoundException] -or $e -is [IO.DirectoryNotFoundException]) { return $false }
        throw
    }
    return (($a -band [IO.FileAttributes]::ReparsePoint) -ne 0)
}

function Test-IemElevatedItem {
    # A folder or file the Highest tasks run, load or write (design section 5.1):
    # no junction or link, owned by Administrators or SYSTEM, and a DACL that
    # grants exactly Get-IemElevatedRights (the user reads only): nothing
    # denied, nobody else. A folder carries its own protected rules (as
    # Set-IemDirectoryAcl writes them, inherited ones refused); a file inherits
    # its folder's, so a file's rules count inherited or explicit, and either
    # way they must be exactly those. Returns the differences.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$UserSid)
    if (Test-IemReparsePoint -Path $Path) { return ,@("$Path is a junction or a link") }
    if (-not (Test-Path -LiteralPath $Path)) { return ,@("$Path does not exist") }
    $acl = Get-Acl -LiteralPath $Path
    $bad = @()
    $owner = $acl.GetOwner([System.Security.Principal.SecurityIdentifier]).Value
    if (@($script:SidAdmins, $script:SidSystem) -notcontains $owner) { $bad += "$Path is owned by $owner" }
    $rights = Get-IemElevatedRights -UserSid $UserSid
    if (Test-Path -LiteralPath $Path -PathType Container) {
        $dirBad = Test-IemDirectoryAcl -Path $Path -Rights $rights
        foreach ($b in $dirBad) { $bad += ('{0}: {1}' -f $Path, $b) }
        return ,$bad
    }
    $seen = @{}
    foreach ($r in @($acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))) {
        $sid = $r.IdentityReference.Value
        if (-not $rights.ContainsKey($sid)) { $bad += "${Path}: a rule for $sid"; continue }
        if ("$($r.AccessControlType)" -ne 'Allow') { $bad += "${Path}: a deny rule for $sid"; continue }
        # An allow rule always carries Synchronize (1048576).
        if ([int]$r.FileSystemRights -ne (([int]$rights[$sid]) -bor 1048576)) { $bad += ('{0}: {1} {2}' -f $Path, $sid, $r.FileSystemRights) }
        if ($seen.ContainsKey($sid)) { $bad += "${Path}: two rules for $sid" }
        $seen[$sid] = $true
    }
    foreach ($sid in $rights.Keys) { if (-not $seen.ContainsKey($sid)) { $bad += "${Path}: no rule for $sid" } }
    return ,$bad
}

function New-IemElevatedSecurity {
    # The elevated root's folders: owner Administrators and a protected DACL
    # (Get-IemElevatedRights) that everything below inherits.
    param([Parameter(Mandatory)][string]$UserSid)
    $sec = New-Object System.Security.AccessControl.DirectorySecurity
    $sec.SetOwner((New-Object System.Security.Principal.SecurityIdentifier $script:SidAdmins))
    $sec.SetAccessRuleProtection($true, $false)
    $rights = Get-IemElevatedRights -UserSid $UserSid
    foreach ($sid in $rights.Keys) {
        $id = New-Object System.Security.Principal.SecurityIdentifier $sid
        $sec.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($id, $rights[$sid], 'ContainerInherit, ObjectInherit', 'None', 'Allow')))
    }
    return $sec
}

function Set-IemAdminsOwner {
    # A file's owner becomes Administrators (its DACL is left alone).
    param([Parameter(Mandatory)][string]$Path)
    if (Test-IemReparsePoint -Path $Path) { throw "$Path is a junction or a link: refused" }
    $fs = New-Object System.Security.AccessControl.FileSecurity
    $fs.SetOwner((New-Object System.Security.Principal.SecurityIdentifier $script:SidAdmins))
    [IO.File]::SetAccessControl($Path, $fs)
}

function Install-IemElevatedFolder {
    # One folder of the elevated root. A new one is created with its owner and
    # DACL in one step (no moment in which another user may add to it). One
    # that exists must be no junction or link and owned by Administrators or
    # SYSTEM, else it is refused (someone else made it: inspect it by hand);
    # its owner and DACL are then set again. Read back.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$UserSid)
    if (Test-IemReparsePoint -Path $Path) { throw "$Path is a junction or a link: refused" }
    $sec = New-IemElevatedSecurity -UserSid $UserSid
    if (Test-Path -LiteralPath $Path -PathType Container) {
        $owner = (Get-Acl -LiteralPath $Path).GetOwner([System.Security.Principal.SecurityIdentifier]).Value
        if (@($script:SidAdmins, $script:SidSystem) -notcontains $owner) {
            throw "$Path exists and is owned by $owner, not Administrators or SYSTEM: refused (inspect it and remove it by hand)"
        }
        [IO.Directory]::SetAccessControl($Path, $sec)
    } else {
        [void][IO.Directory]::CreateDirectory($Path, $sec)
    }
    $bad = Test-IemElevatedItem -Path $Path -UserSid $UserSid
    if ($bad.Count -gt 0) { throw ('elevated folder read-back: ' + ($bad -join '; ')) }
}

function Write-IemElevatedFile {
    # A file of the elevated root, written fresh (an old one is removed first,
    # so none of its rules survive; the new one inherits the folder's), owned
    # by Administrators.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][byte[]]$Bytes)
    if (Test-IemReparsePoint -Path $Path) { throw "$Path is a junction or a link: refused" }
    if (Test-Path -LiteralPath $Path) { Remove-Item -LiteralPath $Path -Force }
    [IO.File]::WriteAllBytes($Path, $Bytes)
    Set-IemAdminsOwner -Path $Path
}

function Install-IemElevatedDir {
    # The elevated root (design section 5.1; %ProgramData%\iemmixer on the PC):
    # tasks\ (this module and the entry script), tasks\out\ (the tasks'
    # results) and tuning\ (S1c's module). Every folder is owned by
    # Administrators with a protected DACL (Administrators and SYSTEM change
    # it, the user only reads), our files are written fresh, and all of it
    # reads back.
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$UserSid)
    $tasks = Join-Path $Root 'tasks'
    foreach ($d in @($Root, $tasks, (Join-Path $tasks 'out'), (Join-Path $Root 'tuning'))) {
        Install-IemElevatedFolder -Path $d -UserSid $UserSid
    }
    $module = Join-Path $tasks 'IemPc.psm1'
    $entry = Join-Path $tasks 'iem-task.ps1'
    if ($module -ne $script:ModuleFile) { Write-IemElevatedFile -Path $module -Bytes ([IO.File]::ReadAllBytes($script:ModuleFile)) }
    Write-IemElevatedFile -Path $entry -Bytes ($script:Utf8NoBom.GetBytes($script:TaskEntry))
    foreach ($f in @($module, $entry)) {
        $bad = Test-IemElevatedItem -Path $f -UserSid $UserSid
        if ($bad.Count -gt 0) { throw ('elevated file read-back: ' + ($bad -join '; ')) }
    }
    $h1 = (Get-FileHash -LiteralPath $script:ModuleFile -Algorithm SHA256).Hash
    $h2 = (Get-FileHash -LiteralPath $module -Algorithm SHA256).Hash
    if ($h1 -cne $h2) { throw "the copy of IemPc.psm1 in $tasks does not match the module" }
    if ([IO.File]::ReadAllText($entry) -cne $script:TaskEntry) { throw "the entry script in $tasks does not read back" }
}

# ---- firewall (P9) ----

function Get-IemFirewallRule {
    # A rule as plain values, or $null.
    param([Parameter(Mandatory)][string]$Name)
    $r = Get-NetFirewallRule -Name $Name -ErrorAction SilentlyContinue
    if ($null -eq $r) { return $null }
    $pf = $r | Get-NetFirewallPortFilter
    [pscustomobject]@{
        direction = "$($r.Direction)"
        action = "$($r.Action)"
        profile = ((@("$($r.Profile)" -split ',\s*') | Sort-Object) -join ',')
        enabled = "$($r.Enabled)"
        protocol = "$($pf.Protocol)"
        ports = ((@($pf.LocalPort) | ForEach-Object { "$_" } | Sort-Object) -join ',')
    }
}

function Test-IemFirewallRule {
    param($Rule, [Parameter(Mandatory)][string]$Enabled)
    if ($null -eq $Rule) { return $false }
    return ($Rule.direction -eq 'Inbound' -and $Rule.action -eq 'Allow' -and $Rule.profile -eq 'Domain,Private' -and
            $Rule.enabled -eq $Enabled -and $Rule.protocol -eq 'TCP' -and $Rule.ports -eq '443,80')
}

function Add-IemFirewallRule {
    # The server's one port rule (P9): TCP 80 and 443 inbound on the private and
    # domain profiles. -Disabled only for the self-test.
    param([string]$Name = 'iemmixer-http', [switch]$Disabled)
    $enabled = 'True'
    if ($Disabled) { $enabled = 'False' }
    $before = Get-IemFirewallRule -Name $Name
    $changed = $false
    if ($null -eq $before) {
        New-NetFirewallRule -Name $Name -DisplayName $Name -Direction Inbound -Action Allow -Protocol TCP -LocalPort 80, 443 `
            -Profile Domain, Private -Enabled $enabled | Out-Null
        $changed = $true
    } elseif (-not (Test-IemFirewallRule -Rule $before -Enabled $enabled)) {
        Set-NetFirewallRule -Name $Name -Direction Inbound -Action Allow -Protocol TCP -LocalPort 80, 443 -Profile Domain, Private -Enabled $enabled
        $changed = $true
    }
    $after = Get-IemFirewallRule -Name $Name
    if (-not (Test-IemFirewallRule -Rule $after -Enabled $enabled)) {
        throw ('firewall rule {0} reads back as {1}' -f $Name, (ConvertTo-Json -InputObject $after -Compress))
    }
    [pscustomobject]@{ name = $Name; changed = $changed; rule = $after }
}

# ---- service rights (S0 hand-off 7: the tunnel's service) ----

function Get-IemServiceSddl {
    param([Parameter(Mandatory)][string]$Service)
    $ErrorActionPreference = 'Continue'
    $out = @(& sc.exe sdshow $Service 2>&1)
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($code -ne 0) { throw ('sc.exe sdshow {0} exited {1}: {2}' -f $Service, $code, ($out -join ' ')) }
    $lines = @($out | ForEach-Object { "$_".Trim() } | Where-Object { $_ -cmatch '^(O:|G:|D:|S:)' })
    if ($lines.Count -ne 1) { throw ('sc.exe sdshow {0}: no security descriptor in "{1}"' -f $Service, ($out -join ' ')) }
    return $lines[0]
}

function Test-IemServiceGrant {
    # Whether a service DACL lets $Sid itself start, stop and query it (allowed,
    # none denied). A grant check, not an exact read-back: a service DACL has no
    # parent to inherit from; an inherit-only ACE (IO) does not apply to the
    # service and is skipped, every other ACE for $Sid counts.
    param([Parameter(Mandatory)][string]$Sddl, [Parameter(Mandatory)][string]$Sid)
    $sd = New-Object System.Security.AccessControl.RawSecurityDescriptor $Sddl
    $allow = 0
    $deny = 0
    if ($null -ne $sd.DiscretionaryAcl) {
        foreach ($ace in $sd.DiscretionaryAcl) {
            if ($ace -isnot [System.Security.AccessControl.CommonAce]) { continue }
            if ($ace.SecurityIdentifier.Value -ne $Sid) { continue }
            if (([int]$ace.AceFlags -band 8) -ne 0) { continue }
            if ($ace.AceQualifier -eq [System.Security.AccessControl.AceQualifier]::AccessAllowed) { $allow = $allow -bor $ace.AccessMask }
            elseif ($ace.AceQualifier -eq [System.Security.AccessControl.AceQualifier]::AccessDenied) { $deny = $deny -bor $ace.AccessMask }
        }
    }
    return ((($allow -band $script:ServiceRights) -eq $script:ServiceRights) -and (($deny -band $script:ServiceRights) -eq 0))
}

function Add-IemServiceAce {
    # The descriptor with one more ACE: (A;;RPWPLO;;;<sid>).
    param([Parameter(Mandatory)][string]$Sddl, [Parameter(Mandatory)][string]$Sid)
    $sd = New-Object System.Security.AccessControl.RawSecurityDescriptor $Sddl
    if ($null -eq $sd.DiscretionaryAcl) { throw 'the service has no DACL' }
    $id = New-Object System.Security.Principal.SecurityIdentifier $Sid
    $ace = New-Object System.Security.AccessControl.CommonAce([System.Security.AccessControl.AceFlags]::None,
        [System.Security.AccessControl.AceQualifier]::AccessAllowed, $script:ServiceRights, $id, $false, $null)
    $sd.DiscretionaryAcl.InsertAce($sd.DiscretionaryAcl.Count, $ace)
    return $sd.GetSddlForm([System.Security.AccessControl.AccessControlSections]::All)
}

function Test-IemServiceRight {
    # Read-only: may the user start, stop and query the service?
    param([Parameter(Mandatory)][string]$Service, [string]$User = '')
    $u = Resolve-IemUser -User $User
    $sddl = Get-IemServiceSddl -Service $Service
    [pscustomobject]@{ service = $Service; user = $u.name; granted = (Test-IemServiceGrant -Sddl $sddl -Sid $u.sid); sddl = $sddl }
}

function Grant-IemServiceRight {
    # Lets the user start, stop and query the service; prints the descriptor
    # before and after.
    param([Parameter(Mandatory)][string]$Service, [string]$User = '')
    $u = Resolve-IemUser -User $User
    $before = Get-IemServiceSddl -Service $Service
    $changed = $false
    if (-not (Test-IemServiceGrant -Sddl $before -Sid $u.sid)) {
        $new = Add-IemServiceAce -Sddl $before -Sid $u.sid
        $ErrorActionPreference = 'Continue'
        $out = @(& sc.exe sdset $Service $new 2>&1)
        $code = $LASTEXITCODE
        $ErrorActionPreference = 'Stop'
        if ($code -ne 0) { throw ('sc.exe sdset {0} exited {1}: {2}' -f $Service, $code, ($out -join ' ')) }
        $changed = $true
    }
    $after = Get-IemServiceSddl -Service $Service
    $granted = Test-IemServiceGrant -Sddl $after -Sid $u.sid
    if (-not $granted) { throw ('service {0}: the grant does not read back ({1})' -f $Service, $after) }
    [pscustomobject]@{ service = $Service; user = $u.name; changed = $changed; granted = $granted; before = $before; after = $after }
}

# ---- bundles and Defender (P5, S1c G4) ----

function Test-IemBundleSums {
    # A bundle directory against its SHA256SUMS, as the guard's bundle.rs: every
    # listed file present with its hash, nothing unlisted (the sums file itself
    # exempt), names a root file or one level under tuning/. Returns the names.
    param([Parameter(Mandatory)][string]$Dir)
    $base = (Resolve-Path -LiteralPath $Dir).ProviderPath.TrimEnd('\')
    $listed = @{}
    foreach ($line in ([IO.File]::ReadAllText((Join-Path $base 'SHA256SUMS')) -split "`n")) {
        $l = $line.TrimEnd("`r")
        if (-not $l) { continue }
        if ($l -cnotmatch '^([0-9a-f]{64})  ([A-Za-z0-9_.-]+(/[A-Za-z0-9_.-]+)?)$') { throw "malformed SHA256SUMS line: $l" }
        $sum = $Matches[1]
        $name = $Matches[2]
        $parts = @($name -split '/')
        if ($name.Contains('..') -or $parts -contains '.' -or $name -eq 'SHA256SUMS' -or ($parts.Count -eq 2 -and $parts[0] -cne 'tuning')) {
            throw "SHA256SUMS name refused: $name"
        }
        if ($listed.ContainsKey($name)) { throw "SHA256SUMS lists $name twice" }
        $listed[$name] = $sum
    }
    $present = @{}
    foreach ($f in (Get-ChildItem -LiteralPath $base -Recurse -File -Force)) {
        $rel = $f.FullName.Substring($base.Length + 1).Replace('\', '/')
        if ($rel -eq 'SHA256SUMS') { continue }
        if (-not $listed.ContainsKey($rel)) { throw "not in SHA256SUMS: $rel" }
        $h = (Get-FileHash -LiteralPath $f.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($h -cne $listed[$rel]) { throw "does not match SHA256SUMS: $rel" }
        $present[$rel] = $true
    }
    foreach ($name in $listed.Keys) { if (-not $present.ContainsKey($name)) { throw "listed in SHA256SUMS but missing: $name" } }
    $names = @($listed.Keys | Sort-Object)
    return ,$names
}

function Get-IemExclusionPlan {
    # Defender process exclusions under <Root>\bundles\: exactly the verified
    # bundle's executables, the kept bundles' stay, every other one goes. An
    # exclusion outside <Root>\bundles\ is never touched.
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$Sha, [string[]]$Want = @(),
          [string[]]$Keep = @(), [string[]]$Current = @())
    $prefix = (Join-Path $Root 'bundles').TrimEnd('\') + '\'
    $add = @()
    $remove = @()
    foreach ($w in $Want) { if (-not ($Current -contains $w)) { $add += $w } }
    foreach ($c in $Current) {
        if (-not $c -or -not $c.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) { continue }
        $dir = @($c.Substring($prefix.Length) -split '\\')[0].ToLowerInvariant()
        if ($dir -ceq $Sha) {
            if (-not ($Want -contains $c)) { $remove += $c }
        } elseif (-not ($Keep -contains $dir)) {
            $remove += $c
        }
    }
    [pscustomobject]@{ add = $add; remove = $remove }
}

function Set-IemDefenderExclusion {
    # S1c G4, run by the exclude task for an activated bundle: re-verifies
    # bundles\<sha>\SHA256SUMS itself, then Defender process exclusions for
    # exactly that directory's executables (full paths); those of bundles that
    # are neither this one nor -Keep (current, previous) go. Never a folder
    # exclusion on the user-writable root (any found is reported).
    [CmdletBinding(SupportsShouldProcess)]
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$Sha, [string[]]$Keep = @())
    foreach ($s in (@($Sha) + @($Keep))) { if ($s -cnotmatch '^[0-9a-f]{40}$') { throw "not a bundle SHA: '$s'" } }
    $bundles = Join-Path $Root 'bundles'
    $dir = Join-Path $bundles $Sha
    # The exclude task runs elevated in the user's root: never through a junction or a link.
    foreach ($p in @($Root, $bundles, $dir)) { if (Test-IemReparsePoint -Path $p) { throw "$p is a junction or a link: refused" } }
    $names = Test-IemBundleSums -Dir $dir
    $want = @($names | Where-Object { $_ -notlike '*/*' -and $_ -like '*.exe' } | ForEach-Object { Join-Path $dir $_ })
    if ($want.Count -eq 0) { throw "bundle $Sha has no executable" }
    $defender = 'available'
    $current = @()
    $folders = @()
    try {
        $pref = Get-MpPreference
        $current = @($pref.ExclusionProcess | Where-Object { $_ })
        $rootPath = $Root.TrimEnd('\')
        $folders = @($pref.ExclusionPath | Where-Object { $_ -and $_.StartsWith($rootPath, [StringComparison]::OrdinalIgnoreCase) })
    } catch {
        if (-not $WhatIfPreference) { throw ('Defender preferences unreadable: ' + $_.Exception.Message) }
        $defender = 'unavailable: ' + $_.Exception.Message
    }
    $plan = Get-IemExclusionPlan -Root $Root -Sha $Sha -Want $want -Keep $Keep -Current $current
    foreach ($p in $plan.remove) { if ($PSCmdlet.ShouldProcess($p, 'remove the Defender process exclusion')) { Remove-MpPreference -ExclusionProcess $p } }
    foreach ($p in $plan.add) { if ($PSCmdlet.ShouldProcess($p, 'add a Defender process exclusion')) { Add-MpPreference -ExclusionProcess $p } }
    if (-not $WhatIfPreference) {
        $now = @((Get-MpPreference).ExclusionProcess | Where-Object { $_ })
        $left = Get-IemExclusionPlan -Root $Root -Sha $Sha -Want $want -Keep $Keep -Current $now
        if ($left.add.Count -gt 0 -or $left.remove.Count -gt 0) {
            throw ('Defender read-back: missing {0}; still excluded {1}' -f ($left.add -join ', '), ($left.remove -join ', '))
        }
    }
    [pscustomobject]@{ sha = $Sha; add = $plan.add; remove = $plan.remove; keep = $Keep; whatif = [bool]$WhatIfPreference
                       defender = $defender; folder_exclusions = $folders }
}

# ---- the HIL runner (design section 7) ----

function Register-IemRunner {
    # The pinned runner, registered on the ops repo with the label iem-pc. The
    # one-time registration token comes only in the process environment variable
    # ACTIONS_RUNNER_INPUT_TOKEN (config.cmd reads it), never on a command line,
    # and is removed right after. No service: the guard starts the runner in dev
    # only. -WhatIf plans without touching anything.
    [CmdletBinding(SupportsShouldProcess)]
    param([Parameter(Mandatory)][string]$Dir, [string]$Zip = '', [string]$Version = '', [string]$Sha256 = '',
          [string]$Url = '', [string]$Label = 'iem-pc')
    if (-not $Version) { $Version = $script:RunnerVersion }
    if (-not $Sha256) { $Sha256 = $script:RunnerSha256 }
    if (-not $Url) { $Url = $script:OpsUrl }
    if ($Version -cnotmatch '^[0-9]+\.[0-9]+\.[0-9]+$') { throw "runner version '$Version' refused" }
    if ($Sha256 -cnotmatch '^[0-9a-f]{64}$') { throw 'the runner zip needs its pinned sha256 (64 lowercase hex)' }
    if ($Url -cnotmatch '^https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') { throw "runner URL '$Url' refused" }
    if ($Label -cnotmatch '^[A-Za-z0-9_-]+$') { throw "runner label '$Label' refused" }
    $Dir = $Dir.TrimEnd('\')
    if (-not $Zip) { $Zip = Join-Path (Split-Path -Parent $Dir) ('actions-runner-win-x64-{0}.zip' -f $Version) }
    $configArgs = @('--unattended', '--url', $Url, '--labels', $Label, '--work', '_work', '--replace')
    $tokenSet = -not [string]::IsNullOrEmpty($env:ACTIONS_RUNNER_INPUT_TOKEN)
    $marker = Join-Path $Dir '.runner'
    if (Test-Path -LiteralPath $marker -PathType Leaf) {
        $cfg = [IO.File]::ReadAllText($marker) | ConvertFrom-Json
        $registered = [string](Get-IemProp $cfg 'gitHubUrl')
        Remove-Item -Path 'Env:\ACTIONS_RUNNER_INPUT_TOKEN' -ErrorAction SilentlyContinue
        if ($registered.TrimEnd('/') -cne $Url) { throw "the runner in $Dir is registered for '$registered', not $Url" }
        return [pscustomobject]@{ dir = $Dir; configured = $true; changed = $false; url = $registered; version = $Version
                                  args = $configArgs; token_present = $tokenSet; whatif = [bool]$WhatIfPreference }
    }
    if (-not $PSCmdlet.ShouldProcess($Dir, "download runner $Version, check its sha256, unpack and register it for $Url")) {
        return [pscustomobject]@{ dir = $Dir; configured = $false; changed = $false; url = $Url; version = $Version; zip = $Zip
                                  args = $configArgs; token_present = $tokenSet; whatif = $true }
    }
    if (-not $tokenSet) { throw 'the one-time registration token is missing from ACTIONS_RUNNER_INPUT_TOKEN' }
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        if (-not (Test-Path -LiteralPath $Zip -PathType Leaf)) {
            $part = $Zip + '.partial'
            $src = 'https://github.com/actions/runner/releases/download/v{0}/actions-runner-win-x64-{0}.zip' -f $Version
            Invoke-WebRequest -UseBasicParsing -Uri $src -OutFile $part
            Move-Item -LiteralPath $part -Destination $Zip -Force
        }
        $h = (Get-FileHash -LiteralPath $Zip -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($h -cne $Sha256) { throw ('runner zip {0} has sha256 {1}, the pin is {2}' -f $Zip, $h, $Sha256) }
        if (-not (Test-Path -LiteralPath $Dir -PathType Container)) { New-Item -ItemType Directory -Force -Path $Dir | Out-Null }
        Expand-Archive -LiteralPath $Zip -DestinationPath $Dir -Force
        $ErrorActionPreference = 'Continue'
        $out = @(& (Join-Path $Dir 'config.cmd') @configArgs 2>&1)
        $code = $LASTEXITCODE
        $ErrorActionPreference = 'Stop'
        if ($code -ne 0) { throw ('config.cmd exited {0}: {1}' -f $code, (@($out | Select-Object -Last 20) -join ' | ')) }
    } finally {
        Remove-Item -Path 'Env:\ACTIONS_RUNNER_INPUT_TOKEN' -ErrorAction SilentlyContinue
    }
    if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) { throw "config.cmd left no $marker" }
    $registered = [string](Get-IemProp ([IO.File]::ReadAllText($marker) | ConvertFrom-Json) 'gitHubUrl')
    if ($registered.TrimEnd('/') -cne $Url) { throw "the runner reads back as registered for '$registered'" }
    [pscustomobject]@{ dir = $Dir; configured = $true; changed = $true; url = $registered; version = $Version
                       args = $configArgs; token_present = $tokenSet; whatif = $false }
}

# ---- read-only facts (design section 6, plan Task 16 Step 1) ----

function ConvertFrom-IemTunnelConfig {
    # The connector's /config (the remotely managed ingress): each rule's
    # hostname and origin. The summary names no host (it may go on #9).
    param([Parameter(Mandatory)]$Config, [string]$Metrics = '')
    $rules = @()
    foreach ($r in @(Get-IemProp (Get-IemProp $Config 'config') 'ingress')) {
        if ($null -eq $r) { continue }
        $service = [string](Get-IemProp $r 'service')
        $row = [ordered]@{ hostname = [string](Get-IemProp $r 'hostname'); service = $service; origin = $false
                           scheme = ''; host = ''; port = 0; loopback = $false }
        if ($service -match '^(https?|wss?|tcp)://') {
            $uri = New-Object System.Uri $service
            $row.origin = $true
            $row.scheme = $uri.Scheme
            $row.host = $uri.Host
            $row.port = $uri.Port
            $row.loopback = ($uri.IsLoopback -or $uri.Host -eq 'localhost')
        }
        $rules += [pscustomobject]$row
    }
    $origins = @($rules | Where-Object { $_.origin })
    $allLoopback = ($origins.Count -gt 0) -and (@($origins | Where-Object { -not $_.loopback }).Count -eq 0)
    $parts = @($origins | ForEach-Object {
        $where = 'remote'
        if ($_.loopback) { $where = 'loopback' }
        '{0} :{1} {2}' -f $_.scheme, $_.port, $where
    })
    [pscustomobject]@{
        metrics = $Metrics
        version = (Get-IemProp $Config 'version')
        rules = $rules
        all_loopback = $allLoopback
        summary = ('{0} rule(s); origins: {1}' -f $rules.Count, ($parts -join ', '))
    }
}

function Get-IemTunnelOrigin {
    # Read-only (G7): the tunnel's origin as the connector reports it on its
    # metrics server (/config). The ingress is never edited.
    param([string]$Metrics = '', [int[]]$Ports = @(20241, 20242, 20243, 20244, 20245))
    $bases = @($Ports | ForEach-Object { 'http://127.0.0.1:{0}' -f $_ })
    if ($Metrics) { $bases = @($Metrics.TrimEnd('/')) }
    $errors = @()
    foreach ($b in $bases) {
        try { $resp = Invoke-WebRequest -UseBasicParsing -Uri ($b + '/config') -TimeoutSec 5 }
        catch { $errors += ('{0}: {1}' -f $b, $_.Exception.Message); continue }
        return (ConvertFrom-IemTunnelConfig -Config ($resp.Content | ConvertFrom-Json) -Metrics $b)
    }
    throw ('no tunnel connector answered /config: ' + ($errors -join '; '))
}

function Get-IemTriggerKind {
    param([Parameter(Mandatory)][int]$Type)
    $names = @{ 0 = 'event'; 1 = 'time'; 2 = 'daily'; 3 = 'weekly'; 4 = 'monthly'; 5 = 'monthly-dow'; 6 = 'idle'
                7 = 'registration'; 8 = 'boot'; 9 = 'logon'; 11 = 'session-state' }
    if ($names.ContainsKey($Type)) { return $names[$Type] }
    return ('type {0}' -f $Type)
}

function Test-IemTriggersMayFire {
    # Can the task start REAPER in dev time? Boot and logon triggers fire only at
    # a boot or a logon (the PC then comes back in event mode); every other
    # enabled trigger can fire at any time.
    param([Parameter(Mandatory)][bool]$Enabled, [object[]]$Triggers = @())
    if (-not $Enabled) { return $false }
    return (@($Triggers | Where-Object { $_.enabled -and @(8, 9) -notcontains $_.type }).Count -gt 0)
}

function Get-IemPredecessorFacts {
    # Read-only: the predecessor's REAPER task (its triggers: can one fire in
    # dev time?) and the app's version resource and exe SHA-256 ([guard]
    # app_exe_sha256). The output holds site values: only non-site parts go on #9.
    param([Parameter(Mandatory)][string]$ReaperTask, [Parameter(Mandatory)][string]$AppExe)
    $i = $ReaperTask.LastIndexOf('\')
    if ($i -lt 0) { throw "task path '$ReaperTask' needs its folder (\folder\name)" }
    $folderPath = $ReaperTask.Substring(0, $i)
    if (-not $folderPath) { $folderPath = '\' }
    $sch = Connect-IemScheduler
    $task = Get-IemRegisteredTask -Scheduler $sch -Folder $folderPath -Name $ReaperTask.Substring($i + 1)
    if ($null -eq $task) { throw "task $ReaperTask does not exist" }
    $def = $task.Definition
    $triggers = @()
    foreach ($t in $def.Triggers) {
        $type = [int]$t.Type
        $triggers += [pscustomobject]@{ type = $type; kind = (Get-IemTriggerKind -Type $type); enabled = [bool]$t.Enabled
                                        start = [string]$t.StartBoundary; end = [string]$t.EndBoundary
                                        repeat = [string]$t.Repetition.Interval }
    }
    $enabled = [bool]$def.Settings.Enabled
    $xml = [xml]$def.XmlText
    $node = $xml.DocumentElement.SelectSingleNode("*[local-name()='Triggers']")
    $triggersXml = ''
    if ($null -ne $node) { $triggersXml = $node.OuterXml }
    $vi = (Get-Item -LiteralPath $AppExe).VersionInfo
    [pscustomobject]@{
        reaper_task = [pscustomobject]@{ enabled = $enabled; state = [int]$task.State; triggers = $triggers
                                         may_fire_in_dev_time = (Test-IemTriggersMayFire -Enabled $enabled -Triggers $triggers)
                                         xml = $triggersXml }
        app = [pscustomobject]@{ file_version = [string]$vi.FileVersion; product_version = [string]$vi.ProductVersion
                                 sha256 = (Get-FileHash -LiteralPath $AppExe -Algorithm SHA256).Hash.ToLowerInvariant() }
    }
}

function Get-IemModuleHolders {
    # Processes that have the driver module loaded, as "image:pid" (I3).
    param([Parameter(Mandatory)][string]$Module)
    if ($Module -cnotmatch '^[^\\/:*?"<>|]+\.dll$') { throw "module name '$Module' refused" }
    $ErrorActionPreference = 'Continue'
    $out = @(& tasklist.exe /m $Module /fo csv /nh 2>&1)
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($code -ne 0) { throw ('tasklist /m {0} exited {1}: {2}' -f $Module, $code, ($out -join ' ')) }
    $rows = @($out | ForEach-Object { "$_" } | Where-Object { $_ -like '"*' } | ForEach-Object {
        $c = $_.Trim('"') -split '","'
        '{0}:{1}' -f $c[0], $c[1]
    })
    return ,$rows
}

function ConvertTo-IemHkcuPath {
    # The site's key (`Software\...`, relative to HKCU) as a PowerShell path.
    param([Parameter(Mandatory)][string]$Key)
    if ($Key -match '^(HKCU:|Registry::)') { return $Key }
    return 'HKCU:\' + $Key.TrimStart('\')
}

function Get-IemPref {
    # The driver's preferred buffer: value, registry kind (DWord or String) and raw text.
    param([Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][string]$Name)
    $item = Get-Item -LiteralPath (ConvertTo-IemHkcuPath -Key $Key)
    $kind = $item.GetValueKind($Name)
    if (@([Microsoft.Win32.RegistryValueKind]::DWord, [Microsoft.Win32.RegistryValueKind]::String) -notcontains $kind) {
        throw "$Name has registry kind $kind (expected DWord or String)"
    }
    $raw = [string]$item.GetValue($Name)
    [pscustomobject]@{ value = [int]$raw; kind = "$kind"; raw = $raw }
}

function Test-IemPrefIsOriginal {
    param([Parameter(Mandatory)]$Pref, [Parameter(Mandatory)][string]$Original)
    if ($Pref.kind -eq 'String') { return ($Pref.raw -ceq $Original) }
    return ($Pref.value -eq [int]$Original)
}

function Restore-IemPref {
    # The preferred buffer back to its recorded original: kind kept, each write
    # read back, three attempts at most (design section 5.2 step 4; G1 at logon).
    param([Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][string]$Original)
    if ($Original -cnotmatch '^[0-9]{1,5}$') { throw "original '$Original' refused (digits)" }
    $path = ConvertTo-IemHkcuPath -Key $Key
    $before = Get-IemPref -Key $Key -Name $Name
    $now = $before
    $attempts = 0
    while (-not (Test-IemPrefIsOriginal -Pref $now -Original $Original) -and $attempts -lt 3) {
        $attempts++
        $data = [int]$Original
        if ($now.kind -eq 'String') { $data = $Original }
        Set-ItemProperty -LiteralPath $path -Name $Name -Value $data -Type $now.kind
        $now = Get-IemPref -Key $Key -Name $Name
    }
    [pscustomobject]@{ before = $before.raw; after = $now.raw; kind = $now.kind; attempts = $attempts
                       ok = (Test-IemPrefIsOriginal -Pref $now -Original $Original) }
}

function Get-IemBootstrapState {
    # Read-only (plan Task 16 Step 1): REAPER and the app running, the driver
    # module's holders, the preference, our tasks and their descriptors, the
    # root's DACL, the firewall rule, the network categories, Defender.
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$Module, [Parameter(Mandatory)][string]$PrefKey,
          [Parameter(Mandatory)][string]$PrefName, [Parameter(Mandatory)][string]$AppImage, [string]$ReaperImage = 'reaper',
          [string]$Folder = '\iemmixer', [string]$FirewallRule = 'iemmixer-http', [string]$User = '')
    $u = Resolve-IemUser -User $User
    $sch = Connect-IemScheduler
    $tasks = @()
    foreach ($name in $script:TaskNames) {
        $t = Get-IemRegisteredTask -Scheduler $sch -Folder $Folder -Name $name
        if ($null -eq $t) { $tasks += [pscustomobject]@{ task = $name; exists = $false; state = 0; last_result = 0; sddl_ok = $false }; continue }
        $sddl = [string]$t.GetSecurityDescriptor(4)
        $tasks += [pscustomobject]@{ task = $name; exists = $true; state = [int]$t.State; last_result = [int64]$t.LastTaskResult
                                     sddl_ok = (Test-IemTaskSddl -Sddl $sddl -UserSid $u.sid) }
    }
    $pref = $null
    $prefError = ''
    try { $pref = Get-IemPref -Key $PrefKey -Name $PrefName } catch { $prefError = $_.Exception.Message }
    $rootExists = Test-Path -LiteralPath $Root -PathType Container
    $rootBad = @()
    if ($rootExists) { $rootBad = Test-IemDirectoryAcl -Path $Root -Rights (Get-IemRootRights -UserSid $u.sid) }
    $fw = Get-IemFirewallRule -Name $FirewallRule
    $defender = 'available'
    try { $null = Get-MpComputerStatus } catch { $defender = 'unavailable: ' + $_.Exception.Message }
    $app = $AppImage -replace '\.exe$', ''
    [pscustomobject]@{
        user = $u.name
        reaper = @(Get-Process -Name $ReaperImage -ErrorAction SilentlyContinue).Count
        app = @(Get-Process -Name $app -ErrorAction SilentlyContinue).Count
        holders = (Get-IemModuleHolders -Module $Module)
        pref = $pref
        pref_error = $prefError
        tasks = $tasks
        root = [pscustomobject]@{ exists = $rootExists; acl_ok = ($rootExists -and $rootBad.Count -eq 0); problems = $rootBad }
        firewall = [pscustomobject]@{ present = ($null -ne $fw); ok = (Test-IemFirewallRule -Rule $fw -Enabled 'True'); rule = $fw }
        networks = @(Get-NetConnectionProfile -ErrorAction SilentlyContinue | ForEach-Object { "$($_.NetworkCategory)" })
        defender = $defender
    }
}

# ---- the elevated tasks' body (design section 5.1) ----

function Get-IemTaskUserSid {
    # The account this task runs as: the user whose rights the elevated root grants.
    return [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
}

function Read-IemTaskRequest {
    # <Root>\guard\tasks\<kind>.request.json, written by the Limited guard in the
    # user's root: never read through a junction or a link, at most 64 KiB, and
    # only its own fields are used.
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$Kind)
    $guard = Join-Path $Root 'guard'
    $dir = Join-Path $guard 'tasks'
    $path = Join-Path $dir ($Kind + '.request.json')
    foreach ($p in @($Root, $guard, $dir, $path)) { if (Test-IemReparsePoint -Path $p) { throw "$p is a junction or a link: refused" } }
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "no request at $path" }
    if ((Get-Item -LiteralPath $path -Force).Length -gt 65536) { throw 'the request is larger than 64 KiB' }
    $doc = $null
    try { $doc = [IO.File]::ReadAllText($path) | ConvertFrom-Json } catch { throw 'the request is not JSON' }
    $id = [string](Get-IemProp $doc 'id')
    if ($id -cnotmatch '^[A-Za-z0-9_.:-]{1,64}$') { throw 'request id refused (1 to 64 of A-Z a-z 0-9 _ . : -)' }
    [pscustomobject]@{ id = $id; doc = $doc }
}

function Invoke-IemTuningVerb {
    # S1c's tuning module: IemTuning.psm1 and profile.json in -TuningDir
    # (<elevated root>\tuning, from the task's command line); 'absent' until S1c
    # ships them. The module runs elevated, so it is imported only when its
    # folder, that folder's parent, the module and the profile are all owned
    # by Administrators or SYSTEM and only they may change them.
    param([Parameter(Mandatory)][string]$Verb, [string]$TuningDir = '')
    if ($script:TuningVerbs -cnotcontains $Verb) { throw "tuning verb '$Verb' refused (enter, exit, state, apply-tier2)" }
    $TuningDir = $TuningDir.TrimEnd('\')
    if (-not $TuningDir -or -not [IO.Path]::IsPathRooted($TuningDir)) { throw 'the tuning folder (-TuningDir) is not an absolute path' }
    $module = Join-Path $TuningDir 'IemTuning.psm1'
    $profilePath = Join-Path $TuningDir 'profile.json'
    if (-not (Test-Path -LiteralPath $module -PathType Leaf) -or -not (Test-Path -LiteralPath $profilePath -PathType Leaf)) { return 'absent' }
    $sid = Get-IemTaskUserSid
    $bad = @()
    foreach ($p in @((Split-Path -Parent $TuningDir), $TuningDir, $module, $profilePath)) {
        $b = Test-IemElevatedItem -Path $p -UserSid $sid
        $bad += $b
    }
    if ($bad.Count -gt 0) { throw ('tuning module refused (not admin-only): ' + ($bad -join '; ')) }
    Import-Module $module -Force
    switch ($Verb) {
        'enter' { return (Enter-IemTuningMode -ProfilePath $profilePath) }
        'exit' { return (Exit-IemTuningMode -ProfilePath $profilePath) }
        'state' { return (Get-IemTuningState -ProfilePath $profilePath) }
        'apply-tier2' { return (Invoke-IemTuningApply -ProfilePath $profilePath -Tier 2) }
    }
}

function Invoke-IemTaskRequest {
    # The body of the Highest tasks. The guard writes <Root>\guard\tasks\
    # <kind>.request.json ({"id", "verb"} for tuning; {"id", "sha", "keep"} for
    # exclude), runs the task and reads -OutDir\<kind>.result.json ({"kind",
    # "id", "ok", "at", "result", "error"}; -OutDir is <elevated root>\tasks\out,
    # which only Administrators and SYSTEM may change, so the elevated write
    # never lands in the user's root). logon (at the user's logon, G1): tuning
    # exit, then the preference back to its original.
    param([Parameter(Mandatory)][ValidateSet('tuning', 'exclude', 'logon')][string]$Kind, [Parameter(Mandatory)][string]$Root,
          [Parameter(Mandatory)][string]$OutDir, [string]$TuningDir = '', [string]$PrefKey = '', [string]$PrefName = '',
          [string]$PrefOriginal = '')
    $outBad = Test-IemElevatedItem -Path $OutDir -UserSid (Get-IemTaskUserSid)
    if ($outBad.Count -gt 0) { throw ('the result folder is refused (not admin-only): ' + ($outBad -join '; ')) }
    $outFile = Join-Path $OutDir ($Kind + '.result.json')
    foreach ($p in @($outFile, ($outFile + '.tmp'))) { if (Test-IemReparsePoint -Path $p) { throw "$p is a junction or a link: refused" } }
    $result = [ordered]@{ kind = $Kind; id = ''; ok = $false; at = (Get-Date).ToUniversalTime().ToString('o'); result = $null; error = '' }
    try {
        switch ($Kind) {
            'tuning' {
                $req = Read-IemTaskRequest -Root $Root -Kind 'tuning'
                $result.id = $req.id
                $result.result = Invoke-IemTuningVerb -Verb ([string](Get-IemProp $req.doc 'verb')) -TuningDir $TuningDir
                $result.ok = $true
            }
            'exclude' {
                $req = Read-IemTaskRequest -Root $Root -Kind 'exclude'
                $result.id = $req.id
                $keep = @(Get-IemProp $req.doc 'keep' | Where-Object { $null -ne $_ } | ForEach-Object { [string]$_ })
                $result.result = Set-IemDefenderExclusion -Root $Root -Sha ([string](Get-IemProp $req.doc 'sha')) -Keep $keep
                $result.ok = $true
            }
            'logon' {
                $result.id = 'logon'
                $tuning = ''
                try { $tuning = Invoke-IemTuningVerb -Verb 'exit' -TuningDir $TuningDir } catch { $tuning = 'failed: ' + $_.Exception.Message }
                if (-not $PrefKey -or -not $PrefName -or -not $PrefOriginal) { throw 'the logon task needs -PrefKey, -PrefName and -PrefOriginal' }
                $pref = Restore-IemPref -Key $PrefKey -Name $PrefName -Original $PrefOriginal
                $result.result = [pscustomobject]@{ tuning = $tuning; pref = $pref }
                $result.ok = [bool]$pref.ok
            }
        }
    } catch {
        $result.error = $_.Exception.Message
    }
    $out = [pscustomobject]$result
    Write-IemJsonFile -Path $outFile -Value $out
    return $out
}

# ---- HIL v1 helpers (hil-v1.ps1, design section 7) ----

function ConvertFrom-IemReply {
    # iemmode prints one JSON object, compact or indented; after a log line the
    # last line is tried. Anything else is $null.
    param([string]$Text = '')
    $t = $Text.Trim().TrimStart([char]0xFEFF)
    if (-not $t) { return $null }
    $doc = $null
    try { $doc = ConvertFrom-Json -InputObject $t } catch {
        $lines = @($t -split "`r?`n" | Where-Object { $_.Trim() })
        if ($lines.Count -eq 0) { return $null }
        try { $doc = ConvertFrom-Json -InputObject $lines[$lines.Count - 1] } catch { return $null }
    }
    if ($doc -is [System.Management.Automation.PSCustomObject]) { return $doc }
    return $null
}

function Invoke-IemMode {
    # One iemmode call: {exit, reply (the JSON object or $null), out, err}. exit
    # is $null when the program did not start.
    param([Parameter(Mandatory)][string]$Exe, [Parameter(Mandatory)][string[]]$Arguments)
    if ($null -eq (Get-Command -Name $Exe -ErrorAction SilentlyContinue)) {
        return [pscustomobject]@{ exit = $null; reply = $null; out = ''; err = "$Exe not found" }
    }
    $ErrorActionPreference = 'Continue'
    $global:LASTEXITCODE = -1
    try {
        $all = @(& $Exe @Arguments 2>&1)
        $code = $global:LASTEXITCODE
    } catch {
        $ErrorActionPreference = 'Stop'
        return [pscustomobject]@{ exit = $null; reply = $null; out = ''; err = $_.Exception.Message }
    }
    $ErrorActionPreference = 'Stop'
    $out = @($all | Where-Object { $_ -isnot [System.Management.Automation.ErrorRecord] } | ForEach-Object { "$_" }) -join "`n"
    $err = @($all | Where-Object { $_ -is [System.Management.Automation.ErrorRecord] } | ForEach-Object { $_.Exception.Message }) -join "`n"
    [pscustomobject]@{ exit = $code; reply = (ConvertFrom-IemReply -Text $out); out = $out; err = $err }
}

function Test-IemModeOk {
    param([Parameter(Mandatory)]$Result)
    return (($Result.exit -eq 0) -and ($null -ne $Result.reply) -and ((Get-IemProp $Result.reply 'ok') -eq $true))
}

function Get-IemModeText {
    # The guard's detail, else the tail of the call's output.
    param([Parameter(Mandatory)]$Result)
    $d = [string](Get-IemProp $Result.reply 'detail')
    if ($d) { return $d }
    $t = ('{0} {1}' -f $Result.err, $Result.out).Trim()
    if ($t.Length -gt 300) { $t = $t.Substring($t.Length - 300) }
    return ('exit {0}: {1}' -f $Result.exit, $t)
}

function Test-IemHilSwitchStarted {
    # The guard left dev or runs a switch: the job ends as cancelled, never success.
    param($Reply)
    if ($null -eq $Reply) { return $false }
    return (([string](Get-IemProp $Reply 'mode') -cne 'dev') -or ($null -ne (Get-IemProp $Reply 'switching')))
}

function Get-IemJson {
    param([Parameter(Mandatory)][string]$Uri, [int]$TimeoutSec = 10)
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    $resp = Invoke-WebRequest -UseBasicParsing -Uri $Uri -TimeoutSec $TimeoutSec
    return ($resp.Content | ConvertFrom-Json)
}

function Test-IemHilInputs {
    # hil.yml's inputs (validated there too) and the script's own place: the
    # verified bundles\<sha>\ directory. Returns the problems.
    param([string]$Sha = '', [string]$Branch = '', [string]$JobRun = '', [string]$Out = '', [string]$ScriptDir = '',
          [double]$TestDbfs = -30, [double]$TestTtl = 10)
    $p = @()
    if ($Sha -cnotmatch '^[0-9a-f]{40}$') { $p += 'Sha: 40 lowercase hex digits' }
    if ($Branch -cnotmatch '^(dev|main)$') { $p += 'Branch: dev or main' }
    if ($JobRun -cnotmatch '^[0-9]{1,20}$') { $p += 'JobRun: the run id (digits)' }
    if (-not $Out) { $p += 'Out: the result.json path' }
    if (-not $ScriptDir -or (Split-Path -Leaf $ScriptDir) -cne $Sha) { $p += 'hil-v1.ps1 runs only from the verified bundles\<sha>\ directory' }
    # The test signal (design section 7): at most -20 dBFS (the engine caps it
    # there too), for a TTL within the guard's (0, 60] s (HIL_MAX_TTL_S).
    if ([double]::IsNaN($TestDbfs) -or [double]::IsInfinity($TestDbfs) -or $TestDbfs -gt -20) { $p += 'TestDbfs: a level of at most -20 dBFS' }
    if ([double]::IsNaN($TestTtl) -or $TestTtl -le 0 -or $TestTtl -gt 60) { $p += 'TestTtl: more than 0 and at most 60 seconds' }
    return ,$p
}

function Test-IemHilVersion {
    # /api/version names the bundle: its git_hash (short, lowercase) is a prefix of the SHA.
    param([Parameter(Mandatory)][string]$Sha, $Version)
    $hash = [string](Get-IemProp $Version 'git_hash')
    $ok = ($hash.Length -ge 7) -and ($hash -cmatch '^[0-9a-f]+$') -and $Sha.StartsWith($hash, [StringComparison]::Ordinal)
    [pscustomobject]@{ ok = $ok; detail = ("git_hash '{0}'" -f $hash) }
}

function Get-IemHilEngineGap {
    # '' when both engine statuses carry the fields, else what is missing.
    param($Before, $After, [Parameter(Mandatory)][string[]]$Fields)
    foreach ($e in @($Before, $After)) {
        if ($null -eq $e) { return 'iemmode status carries no engine status' }
        foreach ($f in $Fields) { if ($null -eq $e.PSObject.Properties[$f]) { return ("the engine status lacks '{0}'" -f $f) } }
    }
    return ''
}

function Test-IemHilEngineUp {
    # The engine status a HIL v1 wait looks for (design section 7): measured frames 32 and
    # callbacks above -Callbacks; with -Sha its build names that bundle, with -Resets its
    # reset count is above that. $false while the guard shows no engine (it comes up: the
    # guard shows one only after its hello and first Status) or a status without the fields.
    param($Engine, [int64]$Callbacks = 0, [string]$Sha = '', $Resets = $null)
    if ($null -eq $Engine) { return $false }
    foreach ($f in @('frames', 'callbacks')) { if ($null -eq $Engine.PSObject.Properties[$f]) { return $false } }
    if ([int64]$Engine.frames -ne 32) { return $false }
    if ([int64]$Engine.callbacks -le $Callbacks) { return $false }
    if ($Sha -and ([string](Get-IemProp $Engine 'build') -cne $Sha)) { return $false }
    if ($null -ne $Resets) {
        $r = Get-IemProp $Engine 'resets'
        if (($null -eq $r) -or ([int64]$r -le [int64]$Resets)) { return $false }
    }
    return $true
}

function Test-IemHilCard {
    # The engine on the card over a window (design section 7): measured frames 32,
    # callbacks advancing at 96 kHz / 32 (3000 a second, 95 % at least), no
    # missed period, no reset, neither parked nor faulted.
    param($Before, $After, [Parameter(Mandatory)][double]$Seconds)
    $gap = Get-IemHilEngineGap -Before $Before -After $After -Fields @('frames', 'callbacks', 'missed', 'resets', 'parked', 'faulted')
    if ($gap) { return [pscustomobject]@{ ok = $false; detail = $gap; numbers = $null } }
    $calls = [int64]$After.callbacks - [int64]$Before.callbacks
    $missed = [int64]$After.missed - [int64]$Before.missed
    $resets = [int64]$After.resets - [int64]$Before.resets
    $floor = [int64][math]::Floor($Seconds * 3000 * 95 / 100)
    $problems = @()
    if ([int]$After.frames -ne 32) { $problems += ('measured frames {0}' -f $After.frames) }
    if ($calls -lt $floor) { $problems += ('callbacks +{0} in {1} s (at least {2})' -f $calls, $Seconds, $floor) }
    if ($missed -ne 0) { $problems += ('missed +{0}' -f $missed) }
    if ($resets -ne 0) { $problems += ('resets +{0}' -f $resets) }
    if ([bool]$After.parked) { $problems += 'parked' }
    if ([bool]$After.faulted) { $problems += 'faulted' }
    $numbers = [pscustomobject]@{ frames = [int]$After.frames; callbacks = $calls; missed = $missed; resets = $resets; seconds = $Seconds }
    $detail = 'frames 32, +{0} callbacks, 0 missed, 0 resets' -f $calls
    if ($problems.Count -gt 0) { $detail = $problems -join '; ' }
    [pscustomobject]@{ ok = ($problems.Count -eq 0); detail = $detail; numbers = $numbers }
}

function Test-IemHilReopen {
    # A forced reopen (design section 7): exactly one more reset, the card back at 32
    # and streaming, neither parked nor faulted.
    param($Before, $After)
    $gap = Get-IemHilEngineGap -Before $Before -After $After -Fields @('frames', 'callbacks', 'resets', 'parked', 'faulted')
    if ($gap) { return [pscustomobject]@{ ok = $false; detail = $gap; numbers = $null } }
    $resets = [int64]$After.resets - [int64]$Before.resets
    $calls = [int64]$After.callbacks - [int64]$Before.callbacks
    $problems = @()
    if ($resets -ne 1) { $problems += ('resets +{0} (expected +1)' -f $resets) }
    if ([int]$After.frames -ne 32) { $problems += ('measured frames {0}' -f $After.frames) }
    if ($calls -le 0) { $problems += 'callbacks do not advance' }
    if ([bool]$After.parked) { $problems += 'parked' }
    if ([bool]$After.faulted) { $problems += 'faulted' }
    $detail = 'one reset, frames 32, +{0} callbacks' -f $calls
    if ($problems.Count -gt 0) { $detail = $problems -join '; ' }
    [pscustomobject]@{ ok = ($problems.Count -eq 0); detail = $detail; numbers = [pscustomobject]@{ resets = $resets; callbacks = $calls } }
}

function Test-IemHilPanic {
    # An injected RT fault (design section 7): the engine exited 70, the guard
    # started exactly one new engine, and that one streams at 32 (callbacks
    # advancing from the first status after the respawn to the next), neither
    # parked nor faulted. $Respawned is $null when no new engine came.
    param($Before, $Respawned, $Later)
    $gap = Get-IemHilEngineGap -Before $Before -After $Before -Fields @('spawns')
    if (-not $gap -and $null -eq $Respawned) { $gap = 'no new engine within the wait' }
    if (-not $gap) { $gap = Get-IemHilEngineGap -Before $Respawned -After $Later -Fields @('frames', 'callbacks', 'parked', 'faulted', 'spawns', 'last_exit') }
    if ($gap) { return [pscustomobject]@{ ok = $false; detail = $gap; numbers = $null } }
    $spawns = [int64]$Later.spawns - [int64]$Before.spawns
    $calls = [int64]$Later.callbacks - [int64]$Respawned.callbacks
    $problems = @()
    if ($spawns -ne 1) { $problems += ('engine starts +{0} (expected +1)' -f $spawns) }
    if ($null -eq $Later.last_exit -or [int64]$Later.last_exit -ne 70) { $problems += ('last exit {0} (expected 70)' -f $Later.last_exit) }
    if ([int]$Later.frames -ne 32) { $problems += ('measured frames {0}' -f $Later.frames) }
    if ($calls -le 0) { $problems += 'the new engine does not stream' }
    if ([bool]$Later.parked) { $problems += 'parked' }
    if ([bool]$Later.faulted) { $problems += 'faulted' }
    $detail = 'exit 70, one respawn, frames 32, +{0} callbacks' -f $calls
    if ($problems.Count -gt 0) { $detail = $problems -join '; ' }
    [pscustomobject]@{ ok = ($problems.Count -eq 0); detail = $detail; numbers = [pscustomobject]@{ spawns = $spawns; callbacks = $calls } }
}

function Get-IemHilPeaks {
    # engine.hil of an engine status as a hashtable: card output (as text) -> peak (linear)
    # since the engine's previous Status. $null when the status carries no 'hil'.
    param($Engine)
    if ($null -eq $Engine -or $null -eq $Engine.PSObject.Properties['hil']) { return $null }
    $peaks = @{}
    foreach ($o in @(Get-IemProp $Engine 'hil')) {
        if ($null -eq $o) { continue }
        $peaks[[string](Get-IemProp $o 'tx')] = [double](Get-IemProp $o 'peak')
    }
    return $peaks
}

function Test-IemHilSilent {
    # Every HIL spare output of the engine status is silent (peak 0 since the previous
    # Status); $false without spare outputs or without the field.
    param($Engine)
    $peaks = Get-IemHilPeaks -Engine $Engine
    if ($null -eq $peaks -or $peaks.Count -eq 0) { return $false }
    foreach ($p in $peaks.Values) { if ($p -ne 0) { return $false } }
    return $true
}

function Test-IemHilHeard {
    # Some HIL spare output of the engine status carried a signal (a peak above 0 since the
    # previous Status); $false without spare outputs or without the field.
    param($Engine)
    $peaks = Get-IemHilPeaks -Engine $Engine
    if ($null -eq $peaks) { return $false }
    foreach ($p in $peaks.Values) { if ($p -gt 0) { return $true } }
    return $false
}

function Test-IemHilSignal {
    # The HIL test signal (design section 7; the owner's decision on #9, 2026-09-28: it goes
    # only to spare card outputs no mix uses, [guard] hil_tx). The engine's Status carries each
    # spare output with its peak since the previous Status (engine.hil). $During are the engine
    # statuses read while the TTL ran: each spare output's loudest peak there is the asked
    # level within 0.5 dB. $After is the status after the TTL: every spare output silent.
    # $During holds every status read from the signal's start until $After, those after
    # the TTL too: a short TTL's signal may show only in a Status that arrives after it.
    param([object[]]$During = @(), $After, [Parameter(Mandatory)][double]$Dbfs)
    $end = Get-IemHilPeaks -Engine $After
    if ($null -eq $end) { return [pscustomobject]@{ ok = $false; detail = "the engine status lacks 'hil'"; numbers = $null } }
    if ($end.Count -eq 0) { return [pscustomobject]@{ ok = $false; detail = 'the engine opened no HIL output ([guard] hil_tx)'; numbers = $null } }
    $loudest = @{}
    foreach ($e in @($During)) {
        $peaks = Get-IemHilPeaks -Engine $e
        if ($null -eq $peaks) { continue }
        foreach ($tx in $peaks.Keys) {
            if (-not $loudest.ContainsKey($tx) -or $peaks[$tx] -gt $loudest[$tx]) { $loudest[$tx] = $peaks[$tx] }
        }
    }
    $inv = [Globalization.CultureInfo]::InvariantCulture
    $problems = @()
    $levels = @()
    $still = 0
    foreach ($tx in $end.Keys) {
        $db = -150.0
        if ($loudest.ContainsKey($tx) -and $loudest[$tx] -gt 0) { $db = 20 * [math]::Log10($loudest[$tx]) }
        $levels += $db
        if ([math]::Abs($db - $Dbfs) -gt 0.5) { $problems += [string]::Format($inv, 'a spare output peaked at {0:0.0} dBFS', $db) }
        if ($end[$tx] -ne 0) { $still++ }
    }
    if ($still -gt 0) { $problems += ('{0} spare output(s) still sound after the TTL' -f $still) }
    $low = ($levels | Measure-Object -Minimum).Minimum
    $high = ($levels | Measure-Object -Maximum).Maximum
    $numbers = [pscustomobject]@{ outputs = $end.Count; dbfs = $Dbfs; lowest_dbfs = $low; highest_dbfs = $high; after = $still }
    $detail = [string]::Format($inv, '{0} spare output(s) at {1:0.0} dBFS (asked {2:0.0}), silent after the TTL', $end.Count, $low, $Dbfs)
    if ($problems.Count -gt 0) { $detail = [string]::Format($inv, 'asked {0:0.0} dBFS: {1}', $Dbfs, ($problems -join '; ')) }
    [pscustomobject]@{ ok = ($problems.Count -eq 0); detail = $detail; numbers = $numbers }
}

function New-IemHilCheck {
    param([Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][bool]$Ok, [string]$Detail = '', $Numbers = $null)
    [pscustomobject]@{ name = $Name; ok = $Ok; detail = $Detail; numbers = $Numbers }
}

function Get-IemHilConclusion {
    # cancelled wins; no check at all is a failure; any failed check is a failure.
    param([object[]]$Checks = @(), [switch]$Cancelled)
    if ($Cancelled) { return 'cancelled' }
    $all = @($Checks)
    if ($all.Count -eq 0) { return 'failure' }
    if (@($all | Where-Object { -not $_.ok }).Count -gt 0) { return 'failure' }
    return 'success'
}

# Why a HIL job was cancelled, as the public summary says it (P6: fixed
# phrases only; the guard's own text goes to result.json's `why`).
$script:HilCancelPhrases = @{ 'left-dev' = 'the guard left dev'; 'not-free' = 'the PC was not free (job-begin refused)' }

function Get-IemHilSummary {
    # The public check-run text (hil/iem-pc on the public repo, P6): the
    # conclusion, the failed checks' names, or a cancel reason's fixed phrase;
    # never free text. Details stay in result.json (the ops artifact).
    param([Parameter(Mandatory)][string]$Conclusion, [object[]]$Checks = @(), [string]$Reason = '')
    $all = @($Checks)
    if ($Conclusion -ceq 'success') { return ('HIL v1 success: {0} checks ok' -f $all.Count) }
    if ($Conclusion -ceq 'cancelled') {
        if (-not $Reason) { return 'HIL v1 cancelled' }
        if (@($script:HilCancelPhrases.Keys) -cnotcontains $Reason) { throw 'cancel reason refused (a code: left-dev or not-free)' }
        return ('HIL v1 cancelled: {0}' -f $script:HilCancelPhrases[$Reason])
    }
    $failed = @($all | Where-Object { -not $_.ok } | ForEach-Object { $_.name })
    $okCount = @($all | Where-Object { $_.ok }).Count
    return ('HIL v1 failure: {0} ({1} of {2} ok)' -f ($failed -join ', '), $okCount, $all.Count)
}

function New-IemHilResult {
    # result.json: `summary` is public (the report job posts it), `why` (a
    # cancelled job's own text) and `checks` stay in the private ops artifact.
    param([Parameter(Mandatory)][string]$Conclusion, [Parameter(Mandatory)][string]$Summary, [string]$Sha = '', [string]$Branch = '',
          [string]$JobRun = '', [string]$Started = '', [object[]]$Checks = @(), [string]$Why = '')
    [pscustomobject]@{ conclusion = $Conclusion; summary = $Summary; why = $Why; sha = $Sha; branch = $Branch; job_run = $JobRun
                       started = $Started; finished = (Get-Date).ToUniversalTime().ToString('o'); checks = @($Checks) }
}

Export-ModuleMember -Function *-Iem*
