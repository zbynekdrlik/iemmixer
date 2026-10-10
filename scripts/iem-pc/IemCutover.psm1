#Requires -Version 5.1
# The cutover's admin-only half (S8 lane 2, design note section 3.2; #11): the
# predecessor's autostarts (its scheduled tasks and Run values) exported,
# disabled and re-enabled, and the guard task's logon trigger. The guard runs
# Limited, so it asks the Highest task \iemmixer\iemmixer-cutover for each of
# them ({"id", "verb", "export"} in <root>\guard\tasks\cutover.request.json,
# the answer in <elevated root>\tasks\out\cutover.result.json, as for the
# other Highest tasks); the rollback (lane 3) re-enables from the same export.
#
# `iempc cutover` runs Install-IemCutover elevated from the admin-only stage
# (iempc_cutover.py): the list of the predecessor's autostarts (site values,
# from the private env, never in this module), the entry script and copies of
# this module and the two it imports from its own folder (IemPc.psm1: the
# elevated root, file and task helpers; S1c's IemTuningStore.psm1: the exact
# registry save and restore) go admin-only into <elevated root>\cutover, and
# the cutover task is registered as Register-IemTasks registers the other
# Highest tasks. The task trusts nothing in the user's root but the request:
# a verb and an export name it checks; what it disables is the admin-only
# list, what it re-enables the admin-only export.
#
# The export (<elevated root>\cutover\autostarts-<since>, the guard's cutover
# start in seconds since the epoch): task-<n>.xml, each task's XML as Task
# Scheduler gives it, UTF-16 with its byte-order mark (the encoding its
# declaration names), and export.json (version, export, at, tasks: [{path,
# enabled, xml, sha256}], run: [{key, name, raw}], raw as Get-IemRegRaw saves
# a value: its kind and its data, or kind absent). Nothing is ended by force
# (I8); no site value lives here (P6).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'IemPc.psm1') -Force -Global
Import-Module (Join-Path $PSScriptRoot 'IemTuningStore.psm1') -Force -Global

# The modules the cutover folder carries: this one and the two it imports.
$script:ModuleFiles = @($PSCommandPath, (Join-Path $PSScriptRoot 'IemPc.psm1'), (Join-Path $PSScriptRoot 'IemTuningStore.psm1'))
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding $false
$script:StateDirName = 'cutover'
$script:ListName = 'autostarts.json'
$script:EntryName = 'iem-cutover.ps1'
$script:ExportFile = 'export.json'
$script:ListVersion = 1
$script:ExportVersion = 1
$script:ExportPattern = '^autostarts-[0-9]{1,20}$'
$script:DefaultFolder = '\iemmixer'
$script:TaskName = 'iemmixer-cutover'
$script:GuardTask = 'iemmixer-guard'
$script:Verbs = @('autostarts-off', 'autostarts-on', 'logon-on', 'logon-off')
# A task of the predecessor: \folder\name, nothing a command line or cmd reads.
$script:TaskPathPattern = '^\\[^"%!^&|<>\r\n\t]*[^"%!^&|<>\r\n\t\\]$'
# A Run value: an HKCU:\ or HKLM:\ key, |, the value's name.
$script:RunPattern = '^(HKCU|HKLM):\\[^|"\r\n]*[^|"\r\n\\]\|[^|"\r\n]+$'
# The kinds Get-IemRegRaw saves.
$script:Kinds = @('absent', 'String', 'ExpandString', 'MultiString', 'DWord', 'QWord', 'Binary')
# Task Scheduler: TASK_TRIGGER_LOGON, TASK_RUNLEVEL_LUA / _HIGHEST, and
# TASK_CREATE_OR_UPDATE with TASK_DONT_ADD_PRINCIPAL_ACE (as Register-IemTasks).
$script:TriggerLogon = 9
$script:RunLevelLimited = 0
$script:RunLevelHighest = 1
$script:TaskDontAddPrincipalAce = 0x10
$script:TaskCreateOrUpdate = 6 -bor $script:TaskDontAddPrincipalAce

# The cutover task's entry, written by Install-IemCutover next to its module
# copies. It never reads an environment variable for a path.
$script:Entry = @'
# iemmixer S8 (#11): the cutover task's entry, written by Install-IemCutover
# into <elevated root>\cutover next to its copies of IemCutover.psm1,
# IemPc.psm1 and IemTuningStore.psm1 (owner Administrators; only
# Administrators and SYSTEM may change it). It reads
# <Root>\guard\tasks\cutover.request.json from the user's root and writes
# <elevated root>\tasks\out\cutover.result.json, which the user may only read.
param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$Folder)
$env:PSModulePath = [IO.Path]::Combine($PSHOME, 'Modules') + ';' + [IO.Path]::Combine([Environment]::GetFolderPath('ProgramFiles'), 'WindowsPowerShell\Modules')
$ErrorActionPreference = 'Stop'
try {
    Import-Module ([IO.Path]::Combine($PSScriptRoot, 'IemCutover.psm1')) -Force
    $r = Invoke-IemCutoverRequest -Root $Root -ElevatedRoot ([IO.Path]::GetDirectoryName($PSScriptRoot)) -Folder $Folder
    if ($r.ok) { exit 0 }
    exit 1
} catch {
    [Console]::Error.WriteLine('iem-cutover: ' + $_.Exception.Message)
    exit 2
}
'@

# ---- paths, the list and its names ----

function Get-IemCutoverPaths {
    param([string]$ElevatedRoot = '')
    $root = Resolve-IemElevatedRoot -ElevatedRoot $ElevatedRoot
    $dir = Join-Path $root $script:StateDirName
    return [pscustomobject]@{ root = $root; dir = $dir; list = (Join-Path $dir $script:ListName); entry = (Join-Path $dir $script:EntryName) }
}

function Get-IemRepeats {
    # The items named more than once (any case).
    param([string[]]$Items = @())
    $seen = @{}
    $twice = @()
    foreach ($x in @($Items)) {
        $k = ([string]$x).ToLowerInvariant()
        if ($seen.ContainsKey($k)) { $twice += $x }
        $seen[$k] = $true
    }
    return ,$twice
}

function Test-IemAutostartList {
    # The predecessor's autostarts as Install-IemCutover takes them: task paths
    # (\folder\name) and Run values (HKCU:\ or HKLM:\ key|name), at least one,
    # none twice. Returns the problems.
    param([string[]]$Tasks = @(), [string[]]$RunValues = @())
    $bad = @()
    if ((@($Tasks).Count + @($RunValues).Count) -eq 0) { $bad += 'no autostart is named' }
    foreach ($t in @($Tasks)) { if ([string]$t -cnotmatch $script:TaskPathPattern) { $bad += "task path '$t' refused (\folder\name)" } }
    foreach ($r in @($RunValues)) { if ([string]$r -cnotmatch $script:RunPattern) { $bad += "Run value '$r' refused (HKCU:\...|name or HKLM:\...|name)" } }
    $twice = Get-IemRepeats -Items $Tasks
    $twice += Get-IemRepeats -Items $RunValues
    foreach ($x in $twice) { $bad += "'$x' is named twice" }
    return ,$bad
}

function Split-IemTaskPath {
    # \folder\sub\name: the folder (\ for a task at the root) and the name.
    param([Parameter(Mandatory)][string]$Path)
    $i = $Path.LastIndexOf('\')
    $folder = $Path.Substring(0, $i)
    if (-not $folder) { $folder = '\' }
    return [pscustomobject]@{ folder = $folder; name = $Path.Substring($i + 1) }
}

function Get-IemTaskByPath {
    # The registered task, or $null.
    param([Parameter(Mandatory)]$Scheduler, [Parameter(Mandatory)][string]$Path)
    $p = Split-IemTaskPath -Path $Path
    return (Get-IemRegisteredTask -Scheduler $Scheduler -Folder $p.folder -Name $p.name)
}

function Split-IemRunValue {
    param([Parameter(Mandatory)][string]$Value)
    $i = $Value.LastIndexOf('|')
    return [pscustomobject]@{ key = $Value.Substring(0, $i); name = $Value.Substring($i + 1) }
}

function Read-IemAutostartList {
    # <elevated root>\cutover\autostarts.json: the root, the folder and the
    # file read back admin-only, this version, a list Test-IemAutostartList passes.
    param([Parameter(Mandatory)]$Paths, [Parameter(Mandatory)][string]$UserSid)
    $bad = @()
    foreach ($p in @($Paths.root, $Paths.dir, $Paths.list)) { $bad += Test-IemElevatedItem -Path $p -UserSid $UserSid }
    if ($bad.Count -gt 0) { throw ('the autostart list is refused (iempc cutover installs it): ' + ($bad -join '; ')) }
    $doc = [IO.File]::ReadAllText($Paths.list, $script:Utf8NoBom) | ConvertFrom-Json
    if ((Get-IemProp $doc 'version') -ne $script:ListVersion) { throw "$($Paths.list): version $(Get-IemProp $doc 'version'), not $($script:ListVersion): refused" }
    $tasks = @(Get-IemProp $doc 'tasks' | Where-Object { $null -ne $_ } | ForEach-Object { [string]$_ })
    $run = @(Get-IemProp $doc 'run' | Where-Object { $null -ne $_ } | ForEach-Object { [string]$_ })
    $listBad = Test-IemAutostartList -Tasks $tasks -RunValues $run
    if ($listBad.Count -gt 0) { throw ("$($Paths.list) is refused: " + ($listBad -join '; ')) }
    return [pscustomobject]@{ tasks = $tasks; run = $run }
}

function Write-IemCutoverFile {
    # One file of the cutover folder, written fresh (owned by Administrators,
    # the folder's rules inherited), read back: admin-only and these bytes.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][byte[]]$Bytes, [Parameter(Mandatory)][string]$UserSid)
    Write-IemElevatedFile -Path $Path -Bytes $Bytes
    $bad = Test-IemElevatedItem -Path $Path -UserSid $UserSid
    if ($bad.Count -gt 0) { throw ('cutover file read-back: ' + ($bad -join '; ')) }
    if ((Get-IemBytesSha256 -Bytes ([IO.File]::ReadAllBytes($Path))) -cne (Get-IemBytesSha256 -Bytes $Bytes)) { throw "$Path does not read back" }
}

# ---- the export ----

function Read-IemAutostartExport {
    # -Dir\export.json, $null when there is none (an export that did not get
    # so far saved nothing and disabled nothing). The folder and every file
    # must read back admin-only (nothing is restored from what someone else
    # could have written), each XML file its sha256, each entry a task path
    # and an enabled state, or a Run value with a kind Set-IemRegRaw writes
    # back: checked whole before anything is restored.
    param([Parameter(Mandatory)][string]$Dir, [Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][string]$UserSid)
    $final = Join-Path $Dir $script:ExportFile
    foreach ($p in @($Dir, $final)) { if (Test-IemReparsePoint -Path $p) { throw "$p is a junction or a link: refused (inspect it by hand)" } }
    if (-not (Test-Path -LiteralPath $final -PathType Leaf)) { return $null }
    $bad = @()
    foreach ($p in @($Dir, $final)) { $bad += Test-IemElevatedItem -Path $p -UserSid $UserSid }
    if ($bad.Count -gt 0) { throw ('the export is refused (inspect it by hand): ' + ($bad -join '; ')) }
    $doc = [IO.File]::ReadAllText($final, $script:Utf8NoBom) | ConvertFrom-Json
    if ((Get-IemProp $doc 'version') -ne $script:ExportVersion -or [string](Get-IemProp $doc 'export') -cne $Name) {
        throw "$final is no export $Name of version $($script:ExportVersion): refused"
    }
    $tasks = @(Get-IemProp $doc 'tasks' | Where-Object { $null -ne $_ })
    foreach ($t in $tasks) {
        $file = [string](Get-IemProp $t 'xml')
        if ($file -cnotmatch '^task-[0-9]+\.xml$') { throw "$final names the file '$file': refused" }
        if ([string](Get-IemProp $t 'path') -cnotmatch $script:TaskPathPattern -or (Get-IemProp $t 'enabled') -isnot [bool]) {
            throw "$final holds a task it cannot restore ($(Get-IemProp $t 'path')): refused"
        }
        $p = Join-Path $Dir $file
        $fb = Test-IemElevatedItem -Path $p -UserSid $UserSid
        if ($fb.Count -gt 0) { throw ('the export is refused (inspect it by hand): ' + ($fb -join '; ')) }
        if ((Get-IemBytesSha256 -Bytes ([IO.File]::ReadAllBytes($p))) -cne [string](Get-IemProp $t 'sha256')) { throw "$p is not the file the export saved: refused" }
    }
    $run = @(Get-IemProp $doc 'run' | Where-Object { $null -ne $_ })
    foreach ($v in $run) {
        $raw = Get-IemProp $v 'raw'
        $kind = [string](Get-IemProp $raw 'kind')
        $ok = ($script:Kinds -ccontains $kind) -and ($kind -ceq 'absent' -or $null -ne $raw.PSObject.Properties['data'])
        if (-not $ok -or ('{0}|{1}' -f (Get-IemProp $v 'key'), (Get-IemProp $v 'name')) -cnotmatch $script:RunPattern) {
            throw "$final holds a value it cannot restore ($(Get-IemProp $v 'key')|$(Get-IemProp $v 'name')): refused"
        }
    }
    return [pscustomobject]@{ export = $Name; dir = $Dir; tasks = $tasks; run = $run }
}

function Export-IemAutostarts {
    # The listed autostarts as they are now, into -Dir (a new admin-only
    # folder): each task's XML exactly as Task Scheduler gives it, its
    # enabled state and the file's sha256; each Run value exactly as
    # Get-IemRegRaw saves it (absent too). export.json goes in last, written
    # beside its place and moved there, then read back (Read-IemAutostartExport).
    param([Parameter(Mandatory)]$List, [Parameter(Mandatory)][string]$Dir, [Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][string]$UserSid)
    $sch = Connect-IemScheduler
    foreach ($path in @($List.tasks)) {
        if ($null -eq (Get-IemTaskByPath -Scheduler $sch -Path $path)) { throw "the task $path does not exist: nothing exported, nothing changed" }
    }
    Install-IemElevatedFolder -Path $Dir -UserSid $UserSid
    $tasks = @()
    $i = 0
    foreach ($path in @($List.tasks)) {
        $i++
        $t = Get-IemTaskByPath -Scheduler $sch -Path $path
        $file = 'task-{0}.xml' -f $i
        $bytes = [byte[]]([Text.Encoding]::Unicode.GetPreamble() + [Text.Encoding]::Unicode.GetBytes([string]$t.Xml))
        Write-IemCutoverFile -Path (Join-Path $Dir $file) -Bytes $bytes -UserSid $UserSid
        $tasks += [pscustomobject][ordered]@{ path = $path; enabled = [bool]$t.Enabled; xml = $file; sha256 = (Get-IemBytesSha256 -Bytes $bytes) }
    }
    $run = @()
    foreach ($v in @($List.run)) {
        $rv = Split-IemRunValue -Value $v
        $run += [pscustomobject][ordered]@{ key = $rv.key; name = $rv.name; raw = (Get-IemRegRaw -Path $rv.key -Name $rv.name) }
    }
    $doc = [pscustomobject][ordered]@{ version = $script:ExportVersion; export = $Name; at = [DateTime]::UtcNow.ToString('o'); tasks = $tasks; run = $run }
    $final = Join-Path $Dir $script:ExportFile
    $tmp = $final + '.tmp'
    Write-IemCutoverFile -Path $tmp -Bytes ($script:Utf8NoBom.GetBytes((ConvertTo-Json -InputObject $doc -Depth 6))) -UserSid $UserSid
    [IO.File]::Move($tmp, $final)
    $back = Read-IemAutostartExport -Dir $Dir -Name $Name -UserSid $UserSid
    if ($null -eq $back) { throw "$final does not read back" }
    return $back
}

function Remove-IemSettingsEnabled {
    # A task's XML without its Task/Settings/Enabled element (whitespace not
    # kept), $null when it does not parse.
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Xml)
    $doc = New-Object System.Xml.XmlDocument
    try { $doc.LoadXml($Xml) } catch { return $null }
    foreach ($s in @($doc.DocumentElement.ChildNodes | Where-Object { $_.LocalName -ceq 'Settings' })) {
        foreach ($e in @($s.ChildNodes | Where-Object { $_.LocalName -ceq 'Enabled' })) { [void]$s.RemoveChild($e) }
    }
    return $doc.OuterXml
}

function Compare-IemTaskXml {
    # A task's XML now against the one an export saved: 'exact', or
    # 'enabled-element' (equal but for the Enabled element of Settings, which
    # Task Scheduler may write out once the state was changed and changed
    # back), else 'differs'.
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Saved, [Parameter(Mandatory)][AllowEmptyString()][string]$Now)
    if ($Saved -ceq $Now) { return 'exact' }
    $a = Remove-IemSettingsEnabled -Xml $Saved
    $b = Remove-IemSettingsEnabled -Xml $Now
    if ($null -ne $a -and $null -ne $b -and $a -ceq $b) { return 'enabled-element' }
    return 'differs'
}

function Disable-IemAutostarts {
    # The cutover's step 2: the list exported into <elevated root>\cutover\
    # <export> (never over an export that exists: refused, nothing changed),
    # then each task disabled and each Run value removed, each read back. A
    # failure after the export puts back what it saved (Enable-IemAutostarts)
    # and says whether that worked.
    param([Parameter(Mandatory)][string]$Export, [string]$ElevatedRoot = '', [Parameter(Mandatory)][string]$UserSid)
    if ($Export -cnotmatch $script:ExportPattern) { throw "export name '$Export' refused" }
    $paths = Get-IemCutoverPaths -ElevatedRoot $ElevatedRoot
    $list = Read-IemAutostartList -Paths $paths -UserSid $UserSid
    $dir = Join-Path $paths.dir $Export
    if ((Test-Path -LiteralPath $dir) -or (Test-IemReparsePoint -Path $dir)) { throw "$dir exists: an export is never overwritten (nothing changed)" }
    $saved = Export-IemAutostarts -List $list -Dir $dir -Name $Export -UserSid $UserSid
    try {
        $sch = Connect-IemScheduler
        foreach ($t in @($saved.tasks)) {
            $path = [string](Get-IemProp $t 'path')
            (Get-IemTaskByPath -Scheduler $sch -Path $path).Enabled = $false
            if ([bool](Get-IemTaskByPath -Scheduler $sch -Path $path).Enabled) { throw "the task $path reads back enabled" }
        }
        foreach ($v in @($saved.run)) {
            $key = [string](Get-IemProp $v 'key')
            $name = [string](Get-IemProp $v 'name')
            Remove-IemRegValue -Path $key -Name $name
            if ([string](Get-IemRegRaw -Path $key -Name $name).kind -cne 'absent') { throw "the value $key|$name reads back present" }
        }
    } catch {
        $why = $_.Exception.Message
        $back = 'what was disabled is enabled again'
        try { [void](Enable-IemAutostarts -Export $Export -ElevatedRoot $ElevatedRoot -UserSid $UserSid) } catch { $back = 'putting it back failed too: ' + $_.Exception.Message }
        throw "$why; $back (the export: $dir)"
    }
    return [pscustomobject]@{ state = 'disabled'; export = $Export; dir = $dir; tasks = @($saved.tasks).Count; values = @($saved.run).Count }
}

function Enable-IemAutostarts {
    # Every autostart <export> saved back as it was (the rollback, lane 3,
    # and the cutover's unwind): each task's enabled state as saved, read back
    # with its XML equal to the saved file's (Compare-IemTaskXml, named per
    # task); each Run value written back exactly (Set-IemRegRaw) unless it is
    # so already, read back. Every task must exist before anything is
    # written. No such export (or one without export.json): 'none', nothing
    # to do. Run again: nothing changes.
    param([Parameter(Mandatory)][string]$Export, [string]$ElevatedRoot = '', [Parameter(Mandatory)][string]$UserSid)
    if ($Export -cnotmatch $script:ExportPattern) { throw "export name '$Export' refused" }
    $paths = Get-IemCutoverPaths -ElevatedRoot $ElevatedRoot
    $dir = Join-Path $paths.dir $Export
    $saved = $null
    if ((Test-Path -LiteralPath $dir) -or (Test-IemReparsePoint -Path $dir)) {
        $bad = @()
        foreach ($p in @($paths.root, $paths.dir)) { $bad += Test-IemElevatedItem -Path $p -UserSid $UserSid }
        if ($bad.Count -gt 0) { throw ('the export is refused (inspect it by hand): ' + ($bad -join '; ')) }
        $saved = Read-IemAutostartExport -Dir $dir -Name $Export -UserSid $UserSid
    }
    if ($null -eq $saved) { return [pscustomobject]@{ state = 'none'; export = $Export; tasks = @(); values = 0 } }
    $sch = Connect-IemScheduler
    foreach ($t in @($saved.tasks)) {
        $path = [string](Get-IemProp $t 'path')
        if ($null -eq (Get-IemTaskByPath -Scheduler $sch -Path $path)) {
            throw "the task $path no longer exists: nothing re-enabled (its saved XML: $(Join-Path $dir ([string](Get-IemProp $t 'xml'))))"
        }
    }
    $states = @()
    foreach ($t in @($saved.tasks)) {
        $path = [string](Get-IemProp $t 'path')
        $want = [bool](Get-IemProp $t 'enabled')
        $task = Get-IemTaskByPath -Scheduler $sch -Path $path
        if ([bool]$task.Enabled -ne $want) { $task.Enabled = $want }
        $now = Get-IemTaskByPath -Scheduler $sch -Path $path
        $xml = [IO.File]::ReadAllText((Join-Path $dir ([string](Get-IemProp $t 'xml'))), [Text.Encoding]::Unicode)
        $same = Compare-IemTaskXml -Saved $xml -Now ([string]$now.Xml)
        if ([bool]$now.Enabled -ne $want -or $same -ceq 'differs') { throw "the task $path does not read back as saved (enabled $([bool]$now.Enabled), XML $same)" }
        $states += [pscustomobject]@{ task = $path; enabled = $want; xml = $same }
    }
    foreach ($v in @($saved.run)) {
        $key = [string](Get-IemProp $v 'key')
        $name = [string](Get-IemProp $v 'name')
        $raw = Get-IemProp $v 'raw'
        if (-not (Test-IemRegRawSame -A (Get-IemRegRaw -Path $key -Name $name) -B $raw)) { Set-IemRegRaw -Path $key -Name $name -Raw $raw }
        if (-not (Test-IemRegRawSame -A (Get-IemRegRaw -Path $key -Name $name) -B $raw)) { throw "the value $key|$name does not read back as saved" }
    }
    return [pscustomobject]@{ state = 'enabled'; export = $Export; tasks = $states; values = @($saved.run).Count }
}

# ---- the guard task's logon trigger ----

function Set-IemGuardLogon {
    # The cutover's step 3 (and the rollback's): the guard task (as
    # Register-IemTasks made it: its read-back must pass first, else nothing
    # changes) with exactly one logon trigger for the user (-On) or none;
    # registered again with our descriptor, read back: the same action, the
    # same settings, the triggers. Already so: 'unchanged'.
    param([Parameter(Mandatory)][bool]$On, [string]$Folder = $script:DefaultFolder, [string]$Name = $script:GuardTask, [string]$User = '')
    $u = Resolve-IemUser -User $User
    $sch = Connect-IemScheduler
    $f = Get-IemTaskFolder -Scheduler $sch -Path $Folder
    $t = Get-IemRegisteredTask -Scheduler $sch -Folder $Folder -Name $Name
    $label = $Folder.TrimEnd('\') + '\' + $Name
    if ($null -eq $f -or $null -eq $t) { throw "$label does not exist (Register-IemTasks makes it): nothing changed" }
    $before = Get-IemTaskReport -Task $t -UserSid $u.sid
    $bad = Test-IemTaskReport -Report $before -RunLevel $script:RunLevelLimited
    if ($bad.Count -gt 0) { throw ("$label is not as Register-IemTasks makes it, nothing changed: " + ($bad -join '; ')) }
    $want = ''
    if ($On) { $want = [string]$script:TriggerLogon }
    if ((@($before.triggers) -join ',') -ceq $want) { return [pscustomobject]@{ state = 'unchanged'; task = $label; logon = $On } }
    $d = $t.Definition
    $d.Triggers.Clear()
    if ($On) {
        $tr = $d.Triggers.Create($script:TriggerLogon)
        $tr.UserId = $u.name
        $tr.Enabled = $true
    }
    $sddl = Get-IemTaskSddl -UserSid $u.sid
    [void]$f.RegisterTaskDefinition($Name, $d, $script:TaskCreateOrUpdate, $u.name, $null, [int]$d.Principal.LogonType, $sddl)
    [void]$f.GetTask($Name).SetSecurityDescriptor($sddl, $script:TaskDontAddPrincipalAce)
    $after = Get-IemTaskReport -Task $f.GetTask($Name) -UserSid $u.sid
    $bad = Test-IemTaskReport -Report $after -RunLevel $script:RunLevelLimited
    if (-not (Test-IemSameActions -A $before.actions -B $after.actions)) { $bad += 'its action changed' }
    if ((@($after.triggers) -join ',') -cne $want) { $bad += ('triggers ' + (@($after.triggers) -join ',')) }
    if ($bad.Count -gt 0) { throw ("$label read-back: " + ($bad -join '; ')) }
    return [pscustomobject]@{ state = 'set'; task = $label; logon = $On }
}

# ---- the task and its install ----

function Invoke-IemCutoverVerb {
    # One verb of the cutover task. The export name is checked here and again
    # by each function.
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Verb, [AllowEmptyString()][string]$Export = '',
          [Parameter(Mandatory)][string]$ElevatedRoot, [string]$Folder = $script:DefaultFolder, [Parameter(Mandatory)][string]$UserSid)
    if ($script:Verbs -cnotcontains $Verb) { throw "cutover verb '$Verb' refused (autostarts-off, autostarts-on, logon-on, logon-off)" }
    if ($Verb -ceq 'autostarts-off') { return (Disable-IemAutostarts -Export $Export -ElevatedRoot $ElevatedRoot -UserSid $UserSid) }
    if ($Verb -ceq 'autostarts-on') { return (Enable-IemAutostarts -Export $Export -ElevatedRoot $ElevatedRoot -UserSid $UserSid) }
    return (Set-IemGuardLogon -On ($Verb -ceq 'logon-on') -Folder $Folder)
}

function Invoke-IemCutoverRequest {
    # The cutover task's body: the request the Limited guard wrote in the
    # user's root (Read-IemTaskRequest: no junction, at most 64 KiB), one verb,
    # the answer in <elevated root>\tasks\out\cutover.result.json ({"kind",
    # "id", "ok", "at", "result", "error"}, as Invoke-IemTaskRequest's), never
    # in the user's root.
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$ElevatedRoot, [string]$Folder = $script:DefaultFolder)
    $sid = Get-IemTaskUserSid
    $outDir = [IO.Path]::Combine($ElevatedRoot, 'tasks', 'out')
    $outBad = Test-IemElevatedItem -Path $outDir -UserSid $sid
    if ($outBad.Count -gt 0) { throw ('the result folder is refused (not admin-only): ' + ($outBad -join '; ')) }
    $outFile = Join-Path $outDir 'cutover.result.json'
    foreach ($p in @($outFile, ($outFile + '.tmp'))) { if (Test-IemReparsePoint -Path $p) { throw "$p is a junction or a link: refused" } }
    $result = [ordered]@{ kind = 'cutover'; id = ''; ok = $false; at = (Get-Date).ToUniversalTime().ToString('o'); result = $null; error = '' }
    try {
        $req = Read-IemTaskRequest -Root $Root -Kind 'cutover'
        $result.id = $req.id
        $result.result = Invoke-IemCutoverVerb -Verb ([string](Get-IemProp $req.doc 'verb')) -Export ([string](Get-IemProp $req.doc 'export')) `
            -ElevatedRoot $ElevatedRoot -Folder $Folder -UserSid $sid
        $result.ok = $true
    } catch {
        $result.error = $_.Exception.Message
    }
    $out = [pscustomobject]$result
    Write-IemJsonFile -Path $outFile -Value $out
    return $out
}

function Install-IemCutover {
    # Run elevated by `iempc cutover` from the admin-only stage, before the
    # guard's cutover. Refused before anything changes: a list
    # Test-IemAutostartList refuses, a task or Run value of it that does not
    # exist now (a typo would disable nothing and restore nothing), an
    # elevated root that is not admin-only or that holds the user's root (or
    # the reverse), no task folder, a module that is not the build iempc
    # checked (-ModuleSha256: the stage is shared). Then <elevated root>\
    # cutover (admin-only) gets the modules, the entry script and the list,
    # each read back, and the cutover task is registered as Register-IemTasks
    # registers the other Highest tasks (Interactive for the user, the user
    # may run it, our descriptor) and read back. Run again: the same.
    param([Parameter(Mandatory)][hashtable]$ModuleSha256, [Parameter(Mandatory)][string]$Root,
          [string[]]$Tasks = @(), [string[]]$RunValues = @(), [string]$Folder = $script:DefaultFolder,
          [string]$ElevatedRoot = '', [string]$User = '')
    $bad = Test-IemAutostartList -Tasks $Tasks -RunValues $RunValues
    if ($bad.Count -gt 0) { throw ('refused, nothing changed: ' + ($bad -join '; ')) }
    $u = Resolve-IemUser -User $User
    $paths = Get-IemCutoverPaths -ElevatedRoot $ElevatedRoot
    $rootBad = Test-IemElevatedItem -Path $paths.root -UserSid $u.sid
    if ($rootBad.Count -gt 0) { throw ('the elevated root is refused (Register-IemTasks makes it): ' + ($rootBad -join '; ')) }
    foreach ($v in @($Root, $paths.root, $Folder)) { [void](Format-IemArg -Value $v) }
    $userRoot = $Root.TrimEnd('\') + '\'
    if (($paths.root + '\').StartsWith($userRoot, [StringComparison]::OrdinalIgnoreCase) -or
        $userRoot.StartsWith($paths.root + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw "the elevated root $($paths.root) and the user's root $Root must not contain each other"
    }
    $sch = Connect-IemScheduler
    $f = Get-IemTaskFolder -Scheduler $sch -Path $Folder
    if ($null -eq $f) { throw "the task folder $Folder does not exist (Register-IemTasks makes it): refused, nothing changed" }
    foreach ($t in @($Tasks)) { if ($null -eq (Get-IemTaskByPath -Scheduler $sch -Path $t)) { throw "the task $t does not exist: refused, nothing changed" } }
    foreach ($r in @($RunValues)) {
        $rv = Split-IemRunValue -Value $r
        if ([string](Get-IemRegRaw -Path $rv.key -Name $rv.name).kind -ceq 'absent') { throw "the value $r does not exist: refused, nothing changed" }
    }
    $files = [ordered]@{}
    $sums = [ordered]@{}
    foreach ($m in $script:ModuleFiles) {
        $leaf = Split-Path -Leaf $m
        $want = [string]$ModuleSha256[$leaf]
        if ($want -cnotmatch '^[0-9a-f]{64}$') { throw "no sha256 for ${leaf}: refused, nothing changed" }
        $bytes = [IO.File]::ReadAllBytes($m)
        $got = Get-IemBytesSha256 -Bytes $bytes
        if ($got -cne $want) { throw "the stage's $leaf (sha256 $got) is not the build iempc checked ($want): refused, nothing changed" }
        $files[(Join-Path $paths.dir $leaf)] = $bytes
        $sums[$leaf] = $got
    }
    $list = [pscustomobject][ordered]@{ version = $script:ListVersion; tasks = @($Tasks); run = @($RunValues) }
    $files[$paths.list] = $script:Utf8NoBom.GetBytes((ConvertTo-Json -InputObject $list -Depth 4))
    $files[$paths.entry] = $script:Utf8NoBom.GetBytes($script:Entry)
    Install-IemElevatedFolder -Path $paths.dir -UserSid $u.sid
    foreach ($p in @($files.Keys)) { Write-IemCutoverFile -Path $p -Bytes $files[$p] -UserSid $u.sid }
    $back = Read-IemAutostartList -Paths $paths -UserSid $u.sid
    if ((@($back.tasks) -join '|') -cne (@($Tasks) -join '|') -or (@($back.run) -join '|') -cne (@($RunValues) -join '|')) { throw "$($paths.list) does not read back" }
    $ps = Join-Path ([Environment]::GetFolderPath('System')) 'WindowsPowerShell\v1.0\powershell.exe'
    $taskArgs = '-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File ' + (Format-IemArg $paths.entry) +
        ' -Root ' + (Format-IemArg $Root) + ' -Folder ' + (Format-IemArg $Folder)
    $d = New-IemTaskDefinition -Scheduler $sch -User $u.name -RunLevel $script:RunLevelHighest -Exe $ps -Arguments $taskArgs `
        -WorkDir $paths.dir -Description ('iemmixer S8: ' + $script:TaskName)
    $sddl = Get-IemTaskSddl -UserSid $u.sid
    [void]$f.RegisterTaskDefinition($script:TaskName, $d, $script:TaskCreateOrUpdate, $u.name, $null, [int]$d.Principal.LogonType, $sddl)
    [void]$f.GetTask($script:TaskName).SetSecurityDescriptor($sddl, $script:TaskDontAddPrincipalAce)
    $rep = Get-IemTaskReport -Task $f.GetTask($script:TaskName) -UserSid $u.sid
    $bad = Test-IemTaskReport -Report $rep -RunLevel $script:RunLevelHighest
    $want = [pscustomobject]@{ path = $ps; arguments = $taskArgs; workdir = $paths.dir }
    if (-not (Test-IemSameActions -A @($want) -B $rep.actions)) { $bad += ('action ' + (ConvertTo-Json -InputObject $rep.actions -Compress)) }
    if (@($rep.triggers).Count -gt 0) { $bad += ('triggers ' + (@($rep.triggers) -join ',')) }
    if ($bad.Count -gt 0) { throw ("cutover task read-back ($Folder\$($script:TaskName)): " + ($bad -join '; ')) }
    return [pscustomobject]@{ state = 'installed'; task = ($Folder.TrimEnd('\') + '\' + $script:TaskName); dir = $paths.dir
                              tasks = @($Tasks); run = @($RunValues); modules = [pscustomobject]$sums }
}

Export-ModuleMember -Function Install-IemCutover, Invoke-IemCutoverRequest, Invoke-IemCutoverVerb, Disable-IemAutostarts,
    Enable-IemAutostarts, Set-IemGuardLogon, Test-IemAutostartList, Compare-IemTaskXml, Read-IemAutostartExport
