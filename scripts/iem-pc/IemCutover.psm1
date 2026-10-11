#Requires -Version 5.1
# The cutover's admin-only half (S8 lane 2, design note section 3.2; #11): the
# predecessor's autostarts (its scheduled tasks, Run values and, since S8 lane
# 6, the shortcuts in the desktop user's Startup folder) exported, disabled
# and re-enabled, and the guard task's logon trigger. The guard runs
# Limited, so it asks the Highest task \iemmixer\iemmixer-cutover for each of
# them ({"id", "verb", "export"} in <root>\guard\tasks\cutover.request.json,
# the answer in <elevated root>\tasks\out\cutover.result.json, as for the
# other Highest tasks); a failed cutover's undo re-enables from the same export
# (there is no rollback: the owner's decision on #11).
#
# `iempc cutover` runs Install-IemCutover elevated from the admin-only stage
# (iempc_cutover.py): the list of the predecessor's autostarts (site values,
# from the private env, never in this module), the entry script and copies of
# this module and the two it imports from its own folder (IemPc.psm1, with its
# siblings since #36: the elevated root, file and task helpers; S1c's
# IemTuningStore.psm1: the exact registry save and restore) go admin-only into
# <elevated root>\cutover, and
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
# a value: its kind and its data, or kind absent; startup: [{name, file,
# sha256}], each shortcut's bytes in startup-<n>.lnk, S8 lane 6, an older
# export without it saved none); restored.json, written by
# Enable-IemAutostarts once everything it saved is back (S8 lane 5: an export
# without it, and a listed task already disabled or Run value already absent,
# refuse a new export and the install). Nothing is ended by force (I8); no
# site value lives here (P6).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'IemPc.psm1') -Force -Global
Import-Module (Join-Path $PSScriptRoot 'IemTuningStore.psm1') -Force -Global

# The modules the cutover folder carries: this one and the two it imports, with
# IemPc.psm1's siblings before it (#36: Get-IemPcModuleFiles, the folder IemPc
# was imported from, this one's).
$script:ModuleFiles = @($PSCommandPath) + @(Get-IemPcModuleFiles) + @((Join-Path $PSScriptRoot 'IemTuningStore.psm1'))
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding $false
$script:StateDirName = 'cutover'
$script:ListName = 'autostarts.json'
$script:EntryName = 'iem-cutover.ps1'
$script:ExportFile = 'export.json'
# The mark Enable-IemAutostarts writes into an export once everything it
# saved is back (S8 lane 5): an export that saved something (export.json)
# without it may hold the predecessor's autostarts disabled.
$script:RestoredFile = 'restored.json'
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
# A shortcut in the user's Startup folder (S8 lane 6): a bare .lnk name, no
# path, no '..', nothing a command line, cmd or a wildcard reads.
$script:StartupPattern = '^(?!.*\.\.)[^\\/:*?"<>|%!^&\x00-\x1f]+\.[Ll][Nn][Kk]$'
# A shortcut's copy in an export.
$script:StartupFilePattern = '^startup-[0-9]+\.lnk$'
# Where Windows records each account's profile folder (only Administrators and
# SYSTEM may change it): the anchor a Startup folder must lie in.
$script:ProfileList = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList'
# %SystemDrive%, as a profile path may begin.
$script:SystemDriveVar = '%SystemDrive%'
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
    # (\folder\name), Run values (HKCU:\ or HKLM:\ key|name) and shortcuts in
    # the user's Startup folder (a bare name.lnk), at least one, none twice.
    # Returns the problems.
    param([string[]]$Tasks = @(), [string[]]$RunValues = @(), [string[]]$Startup = @())
    $bad = @()
    if ((@($Tasks).Count + @($RunValues).Count + @($Startup).Count) -eq 0) { $bad += 'no autostart is named' }
    foreach ($t in @($Tasks)) { if ([string]$t -cnotmatch $script:TaskPathPattern) { $bad += "task path '$t' refused (\folder\name)" } }
    foreach ($r in @($RunValues)) { if ([string]$r -cnotmatch $script:RunPattern) { $bad += "Run value '$r' refused (HKCU:\...|name or HKLM:\...|name)" } }
    foreach ($s in @($Startup)) { if ([string]$s -cnotmatch $script:StartupPattern) { $bad += "shortcut '$s' refused (a bare name.lnk in the Startup folder)" } }
    $twice = Get-IemRepeats -Items $Tasks
    $twice += Get-IemRepeats -Items $RunValues
    $twice += Get-IemRepeats -Items $Startup
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
    # S8 lane 6: a list written before it names no shortcut.
    $startup = @(Get-IemProp $doc 'startup' | Where-Object { $null -ne $_ } | ForEach-Object { [string]$_ })
    $listBad = Test-IemAutostartList -Tasks $tasks -RunValues $run -Startup $startup
    if ($listBad.Count -gt 0) { throw ("$($Paths.list) is refused: " + ($listBad -join '; ')) }
    return [pscustomobject]@{ tasks = $tasks; run = $run; startup = $startup }
}

# ---- the user's Startup folder (S8 lane 6) ----

function Get-IemProfileDir {
    # The profile folder of the account -UserSid as Windows records it
    # (ProfileList, which only Administrators and SYSTEM may change), never
    # from an environment variable or the user's HKCU. Only a literal path,
    # or one that begins with %SystemDrive% (the drive of the system
    # directory, which no environment gives), is taken.
    param([Parameter(Mandatory)][string]$UserSid)
    if ($UserSid -cnotmatch '^S-1-[0-9]+(-[0-9]+)+$') { throw "'$UserSid' is no SID: refused" }
    $key = Get-Item -LiteralPath (Join-Path $script:ProfileList $UserSid)
    try { $raw = [string]$key.GetValue('ProfileImagePath', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) } finally { $key.Close() }
    if ($raw.StartsWith($script:SystemDriveVar, [StringComparison]::OrdinalIgnoreCase)) {
        $raw = [IO.Path]::GetPathRoot([Environment]::SystemDirectory).TrimEnd('\') + $raw.Substring($script:SystemDriveVar.Length)
    }
    if (-not $raw -or $raw.Contains('%') -or -not [IO.Path]::IsPathRooted($raw)) { throw "the profile of $UserSid is recorded as '$raw': refused" }
    return [IO.Path]::GetFullPath($raw).TrimEnd('\')
}

function Resolve-IemStartupDir {
    # The user's Startup folder: [Environment]::GetFolderPath('Startup') in
    # this session (the cutover task runs as the user, Interactive; the
    # install in the user's elevated ssh session), never an environment
    # variable or the request; -StartupDir only from the self-test, checked
    # the same way. The user may move the Startup folder (HKCU's shell
    # folders), so it must lie inside the user's profile (Get-IemProfileDir),
    # with no junction or link from the profile folder down to it, and exist.
    param([Parameter(Mandatory)][string]$UserSid, [string]$StartupDir = '')
    $dir = $StartupDir
    if (-not $dir) { $dir = [Environment]::GetFolderPath('Startup') }
    if (-not $dir -or -not [IO.Path]::IsPathRooted($dir)) { throw "this session's Startup folder is '$dir': refused" }
    $dir = [IO.Path]::GetFullPath($dir).TrimEnd('\')
    $profileDir = Get-IemProfileDir -UserSid $UserSid
    if (-not $dir.StartsWith($profileDir + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw "the Startup folder $dir is not inside the profile $profileDir of ${UserSid}: refused"
    }
    $p = $dir
    while ($p.Length -ge $profileDir.Length) {
        if (Test-IemReparsePoint -Path $p) { throw "$p is a junction or a link: the Startup folder is refused" }
        $p = [IO.Path]::GetDirectoryName($p)
        if ($null -eq $p) { break }
    }
    if (-not (Test-Path -LiteralPath $dir -PathType Container)) { throw "the Startup folder $dir does not exist: refused" }
    return $dir
}

function Read-IemShortcut {
    # A shortcut's bytes in the Startup folder -Dir, $null when there is none.
    # The name must be a bare name.lnk; no junction or link, a file.
    param([Parameter(Mandatory)][string]$Dir, [Parameter(Mandatory)][string]$Name)
    if ($Name -cnotmatch $script:StartupPattern) { throw "shortcut '$Name' refused" }
    $path = Join-Path $Dir $Name
    if (Test-IemReparsePoint -Path $path) { throw "$path is a junction or a link: refused" }
    if (-not (Test-Path -LiteralPath $path)) { return $null }
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "$path is no file: refused" }
    return ,([IO.File]::ReadAllBytes($path))
}

function Resolve-IemListedStartup {
    # The Startup folder when -Names names a shortcut, else '' (nothing read).
    param([string[]]$Names = @(), [Parameter(Mandatory)][string]$UserSid, [string]$StartupDir = '')
    if (@($Names).Count -eq 0) { return '' }
    return (Resolve-IemStartupDir -UserSid $UserSid -StartupDir $StartupDir)
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

# ---- what an earlier cutover left (S8 lane 5) ----

function Get-IemUnrestoredExports {
    # The exports in -CutoverDir that saved something (export.json) and were
    # never restored (no restored.json), but -Except.
    param([Parameter(Mandatory)][string]$CutoverDir, [string]$Except = '')
    $names = @()
    if (-not (Test-Path -LiteralPath $CutoverDir -PathType Container)) { return ,$names }
    foreach ($d in @(Get-ChildItem -LiteralPath $CutoverDir -Directory -Force)) {
        $n = [string]$d.Name
        if ($n -cnotmatch $script:ExportPattern -or $n -ceq $Except) { continue }
        $saved = Test-Path -LiteralPath (Join-Path $d.FullName $script:ExportFile) -PathType Leaf
        $restored = Test-Path -LiteralPath (Join-Path $d.FullName $script:RestoredFile)
        if ($saved -and -not $restored) { $names += $n }
    }
    return ,$names
}

function Get-IemAutostartRefusals {
    # What a cutover never undone leaves (its prod lost to trial: an
    # older guard took over, or the guard's state could not be read), which a
    # new export would save as the predecessor's state, so that its undo
    # would restore them disabled: a listed task already disabled, a listed
    # Run value or shortcut (in -StartupDir, Resolve-IemListedStartup's)
    # already absent, an earlier export never restored; an empty shortcut too
    # (nothing to save). A listed task that does not exist is the caller's
    # own refusal. Returns the problems; they are put back by hand
    # (Enable-IemAutostarts from that export) before a cutover.
    param([Parameter(Mandatory)]$List, [Parameter(Mandatory)]$Scheduler, [Parameter(Mandatory)][string]$CutoverDir, [string]$Except = '',
          [string]$StartupDir = '')
    $bad = @()
    foreach ($path in @($List.tasks)) {
        $t = Get-IemTaskByPath -Scheduler $Scheduler -Path $path
        if ($null -ne $t -and -not [bool]$t.Enabled) { $bad += "the task $path is already disabled" }
    }
    foreach ($v in @($List.run)) {
        $rv = Split-IemRunValue -Value $v
        if ([string](Get-IemRegRaw -Path $rv.key -Name $rv.name).kind -ceq 'absent') { $bad += "the value $($rv.key)|$($rv.name) is absent" }
    }
    foreach ($n in @($List.startup)) {
        if (-not $StartupDir) { throw "the shortcut $n is listed and no Startup folder was resolved" }
        $b = Read-IemShortcut -Dir $StartupDir -Name $n
        if ($null -eq $b) { $bad += "the shortcut $n is absent from the Startup folder" }
        elseif ($b.Length -eq 0) { $bad += "the shortcut $n is empty" }
    }
    $unrestored = Get-IemUnrestoredExports -CutoverDir $CutoverDir -Except $Except
    foreach ($n in @($unrestored)) { $bad += "the export $n of an earlier cutover was never restored" }
    return ,$bad
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
    # S8 lane 6: each shortcut's copy, admin-only and its sha256 (an export
    # written before it saved none).
    $startup = @(Get-IemProp $doc 'startup' | Where-Object { $null -ne $_ })
    foreach ($s in $startup) {
        $file = [string](Get-IemProp $s 'file')
        if ($file -cnotmatch $script:StartupFilePattern) { throw "$final names the file '$file': refused" }
        if ([string](Get-IemProp $s 'name') -cnotmatch $script:StartupPattern -or [string](Get-IemProp $s 'sha256') -cnotmatch '^[0-9a-f]{64}$') {
            throw "$final holds a shortcut it cannot restore ($(Get-IemProp $s 'name')): refused"
        }
        $p = Join-Path $Dir $file
        $fb = Test-IemElevatedItem -Path $p -UserSid $UserSid
        if ($fb.Count -gt 0) { throw ('the export is refused (inspect it by hand): ' + ($fb -join '; ')) }
        if ((Get-IemBytesSha256 -Bytes ([IO.File]::ReadAllBytes($p))) -cne [string](Get-IemProp $s 'sha256')) { throw "$p is not the file the export saved: refused" }
    }
    $twice = Get-IemRepeats -Items @($startup | ForEach-Object { [string](Get-IemProp $_ 'name') })
    if ($twice.Count -gt 0) { throw "$final names the shortcut $($twice[0]) twice: refused" }
    return [pscustomobject]@{ export = $Name; dir = $Dir; tasks = $tasks; run = $run; startup = $startup }
}

function Export-IemAutostarts {
    # The listed autostarts as they are now, into -Dir (a new admin-only
    # folder): each task's XML exactly as Task Scheduler gives it, its
    # enabled state and the file's sha256; each Run value exactly as
    # Get-IemRegRaw saves it (absent too); each shortcut's bytes (in
    # -StartupDir, Resolve-IemListedStartup's) as startup-<n>.lnk with its
    # sha256 (S8 lane 6). export.json goes in last, written beside its place
    # and moved there, then read back (Read-IemAutostartExport).
    # Refused before anything is written: Get-IemAutostartRefusals (S8 lane 5).
    param([Parameter(Mandatory)]$List, [Parameter(Mandatory)][string]$Dir, [Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][string]$UserSid,
          [string]$StartupDir = '')
    $sch = Connect-IemScheduler
    foreach ($path in @($List.tasks)) {
        if ($null -eq (Get-IemTaskByPath -Scheduler $sch -Path $path)) { throw "the task $path does not exist: nothing exported, nothing changed" }
    }
    # S8 lane 5: never an export of what an earlier cutover left disabled.
    $refused = Get-IemAutostartRefusals -List $List -Scheduler $sch -CutoverDir (Split-Path -Parent $Dir) -Except $Name -StartupDir $StartupDir
    if ($refused.Count -gt 0) {
        throw (($refused -join '; ') + ': an earlier cutover was never undone (put them back by hand first); nothing exported, nothing changed')
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
    $startup = @()
    $k = 0
    foreach ($n in @($List.startup)) {
        $k++
        $bytes = Read-IemShortcut -Dir $StartupDir -Name $n
        if ($null -eq $bytes -or $bytes.Length -eq 0) { throw "the shortcut $n is gone or empty since the check: nothing disabled" }
        $file = 'startup-{0}.lnk' -f $k
        Write-IemCutoverFile -Path (Join-Path $Dir $file) -Bytes $bytes -UserSid $UserSid
        $startup += [pscustomobject][ordered]@{ name = $n; file = $file; sha256 = (Get-IemBytesSha256 -Bytes $bytes) }
    }
    $doc = [pscustomobject][ordered]@{ version = $script:ExportVersion; export = $Name; at = [DateTime]::UtcNow.ToString('o'); tasks = $tasks; run = $run
                                       startup = $startup }
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
    # then each task disabled, each Run value removed and each shortcut
    # removed from the Startup folder (only once the export read back, and
    # only while it still holds the bytes the export saved), each read back.
    # A failure after the export puts back what it saved
    # (Enable-IemAutostarts) and says whether that worked. -StartupDir: the
    # self-test's Startup folder only (the task's entry never passes it).
    param([Parameter(Mandatory)][string]$Export, [string]$ElevatedRoot = '', [Parameter(Mandatory)][string]$UserSid, [string]$StartupDir = '')
    if ($Export -cnotmatch $script:ExportPattern) { throw "export name '$Export' refused" }
    $paths = Get-IemCutoverPaths -ElevatedRoot $ElevatedRoot
    $list = Read-IemAutostartList -Paths $paths -UserSid $UserSid
    $dir = Join-Path $paths.dir $Export
    if ((Test-Path -LiteralPath $dir) -or (Test-IemReparsePoint -Path $dir)) { throw "$dir exists: an export is never overwritten (nothing changed)" }
    $startupDir = Resolve-IemListedStartup -Names $list.startup -UserSid $UserSid -StartupDir $StartupDir
    $saved = Export-IemAutostarts -List $list -Dir $dir -Name $Export -UserSid $UserSid -StartupDir $startupDir
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
        foreach ($s in @($saved.startup)) {
            $name = [string](Get-IemProp $s 'name')
            $now = Read-IemShortcut -Dir $startupDir -Name $name
            if ($null -eq $now -or (Get-IemBytesSha256 -Bytes $now) -cne [string](Get-IemProp $s 'sha256')) {
                throw "the shortcut $name is not the one the export saved: not removed"
            }
            [IO.File]::Delete((Join-Path $startupDir $name))
            if ($null -ne (Read-IemShortcut -Dir $startupDir -Name $name)) { throw "the shortcut $name reads back present" }
        }
    } catch {
        $why = $_.Exception.Message
        $back = 'what was disabled is enabled again'
        try { [void](Enable-IemAutostarts -Export $Export -ElevatedRoot $ElevatedRoot -UserSid $UserSid -StartupDir $StartupDir) } catch { $back = 'putting it back failed too: ' + $_.Exception.Message }
        throw "$why; $back (the export: $dir)"
    }
    return [pscustomobject]@{ state = 'disabled'; export = $Export; dir = $dir; tasks = @($saved.tasks).Count; values = @($saved.run).Count
                              shortcuts = @($saved.startup).Count }
}

function Enable-IemAutostarts {
    # Every autostart <export> saved back as it was (a failed cutover's undo,
    # at its unwind or a later start): each task's enabled state as saved, read back
    # with its XML equal to the saved file's (Compare-IemTaskXml, named per
    # task); each Run value written back exactly (Set-IemRegRaw) unless it is
    # so already, read back; each shortcut written back into the Startup
    # folder from its copy (its sha256 checked again) and read back, unless
    # it is there with those bytes (S8 lane 6). Every task must exist, and
    # every shortcut be absent or those bytes (one with other bytes is
    # refused, never overwritten), before anything is written. Then the
    # export's restored.json (S8 lane 5), unless it is there. No such export
    # (or one without export.json): 'none', nothing to do. Run again: nothing
    # changes. -StartupDir: the self-test's Startup folder only.
    param([Parameter(Mandatory)][string]$Export, [string]$ElevatedRoot = '', [Parameter(Mandatory)][string]$UserSid, [string]$StartupDir = '')
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
    # S8 lane 6: every shortcut absent or the saved bytes, before anything is written.
    $names = @($saved.startup | ForEach-Object { [string](Get-IemProp $_ 'name') })
    $startupDir = Resolve-IemListedStartup -Names $names -UserSid $UserSid -StartupDir $StartupDir
    $shortcuts = @()
    foreach ($s in @($saved.startup)) {
        $name = [string](Get-IemProp $s 'name')
        $sum = [string](Get-IemProp $s 'sha256')
        $copy = Join-Path $dir ([string](Get-IemProp $s 'file'))
        $bytes = [IO.File]::ReadAllBytes($copy)
        if ((Get-IemBytesSha256 -Bytes $bytes) -cne $sum) { throw "$copy is not the file the export saved: nothing re-enabled" }
        $now = Read-IemShortcut -Dir $startupDir -Name $name
        $there = $null -ne $now
        if ($there -and (Get-IemBytesSha256 -Bytes $now) -cne $sum) {
            throw "the shortcut $name is in the Startup folder with other bytes than the export saved: refused, never overwritten, nothing re-enabled (the saved one: $copy)"
        }
        $shortcuts += [pscustomobject]@{ name = $name; sum = $sum; bytes = $bytes; there = $there }
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
    $restored = @()
    foreach ($s in $shortcuts) {
        $path = Join-Path $startupDir $s.name
        if (-not $s.there) {
            if (Test-IemReparsePoint -Path $path) { throw "$path is a junction or a link: refused" }
            # CreateNew: a file that appeared since the check is never overwritten.
            $fs = [IO.File]::Open($path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            try { $fs.Write($s.bytes, 0, $s.bytes.Length) } finally { $fs.Dispose() }
        }
        $back = Read-IemShortcut -Dir $startupDir -Name $s.name
        if ($null -eq $back -or (Get-IemBytesSha256 -Bytes $back) -cne $s.sum) { throw "the shortcut $($s.name) does not read back as saved" }
        $how = 'written'
        if ($s.there) { $how = 'kept' }
        $restored += [pscustomobject]@{ shortcut = $s.name; state = $how }
    }
    # S8 lane 5: everything is back; a later cutover may export again.
    $mark = Join-Path $dir $script:RestoredFile
    if (Test-IemReparsePoint -Path $mark) { throw "$mark is a junction or a link: refused (inspect it by hand)" }
    if (-not (Test-Path -LiteralPath $mark -PathType Leaf)) {
        $doc = [pscustomobject][ordered]@{ version = $script:ExportVersion; export = $Export; at = [DateTime]::UtcNow.ToString('o') }
        Write-IemCutoverFile -Path $mark -Bytes ($script:Utf8NoBom.GetBytes((ConvertTo-Json -InputObject $doc))) -UserSid $UserSid
    }
    return [pscustomobject]@{ state = 'enabled'; export = $Export; tasks = $states; values = @($saved.run).Count; shortcuts = $restored }
}

# ---- the guard task's logon trigger ----

function Set-IemGuardLogon {
    # The cutover's step 3 (and its undo): the guard task (as
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
    # by each function. -StartupDir: the self-test's only (never the entry's).
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Verb, [AllowEmptyString()][string]$Export = '',
          [Parameter(Mandatory)][string]$ElevatedRoot, [string]$Folder = $script:DefaultFolder, [Parameter(Mandatory)][string]$UserSid,
          [string]$StartupDir = '')
    if ($script:Verbs -cnotcontains $Verb) { throw "cutover verb '$Verb' refused (autostarts-off, autostarts-on, logon-on, logon-off)" }
    if ($Verb -ceq 'autostarts-off') { return (Disable-IemAutostarts -Export $Export -ElevatedRoot $ElevatedRoot -UserSid $UserSid -StartupDir $StartupDir) }
    if ($Verb -ceq 'autostarts-on') { return (Enable-IemAutostarts -Export $Export -ElevatedRoot $ElevatedRoot -UserSid $UserSid -StartupDir $StartupDir) }
    return (Set-IemGuardLogon -On ($Verb -ceq 'logon-on') -Folder $Folder)
}

function Invoke-IemCutoverRequest {
    # The cutover task's body: the request the Limited guard wrote in the
    # user's root (Read-IemTaskRequest: no junction, at most 64 KiB), one verb,
    # the answer in <elevated root>\tasks\out\cutover.result.json ({"kind",
    # "id", "ok", "at", "result", "error"}, as Invoke-IemTaskRequest's), never
    # in the user's root. The user's Startup folder is this session's
    # (Resolve-IemStartupDir), never the request's; -StartupDir is the
    # self-test's only, which the entry script never passes.
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$ElevatedRoot, [string]$Folder = $script:DefaultFolder,
          [string]$StartupDir = '')
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
            -ElevatedRoot $ElevatedRoot -Folder $Folder -UserSid $sid -StartupDir $StartupDir
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
    # Test-IemAutostartList refuses, a task, Run value or shortcut (in this
    # session's Startup folder, Resolve-IemStartupDir; -StartupDir only from
    # the self-test) of it that does not exist now (a typo would disable
    # nothing and restore nothing), what an
    # earlier cutover never undone left (Get-IemAutostartRefusals), an
    # elevated root that is not admin-only or that holds the user's root (or
    # the reverse), no task folder, a module that is not the build iempc
    # checked (-ModuleSha256: the stage is shared). Then <elevated root>\
    # cutover (admin-only) gets the modules, the entry script and the list,
    # each read back, and the cutover task is registered as Register-IemTasks
    # registers the other Highest tasks (Interactive for the user, the user
    # may run it, our descriptor) and read back. Run again: the same.
    param([Parameter(Mandatory)][hashtable]$ModuleSha256, [Parameter(Mandatory)][string]$Root,
          [string[]]$Tasks = @(), [string[]]$RunValues = @(), [string[]]$Startup = @(), [string]$Folder = $script:DefaultFolder,
          [string]$ElevatedRoot = '', [string]$User = '', [string]$StartupDir = '')
    $bad = Test-IemAutostartList -Tasks $Tasks -RunValues $RunValues -Startup $Startup
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
    # S8 lane 6: the user's Startup folder, resolved in this session as the task resolves it.
    $startupDir = Resolve-IemListedStartup -Names $Startup -UserSid $u.sid -StartupDir $StartupDir
    foreach ($n in @($Startup)) {
        if ($null -eq (Read-IemShortcut -Dir $startupDir -Name $n)) { throw "the shortcut $n does not exist in the Startup folder ${startupDir}: refused, nothing changed" }
    }
    # S8 lane 5: never a cutover over what an earlier one left disabled.
    $refused = Get-IemAutostartRefusals -List ([pscustomobject]@{ tasks = @($Tasks); run = @($RunValues); startup = @($Startup) }) -Scheduler $sch `
        -CutoverDir $paths.dir -StartupDir $startupDir
    if ($refused.Count -gt 0) {
        throw (($refused -join '; ') + ': an earlier cutover was never undone (put them back by hand first); refused, nothing changed')
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
    $list = [pscustomobject][ordered]@{ version = $script:ListVersion; tasks = @($Tasks); run = @($RunValues); startup = @($Startup) }
    $files[$paths.list] = $script:Utf8NoBom.GetBytes((ConvertTo-Json -InputObject $list -Depth 4))
    $files[$paths.entry] = $script:Utf8NoBom.GetBytes($script:Entry)
    Install-IemElevatedFolder -Path $paths.dir -UserSid $u.sid
    foreach ($p in @($files.Keys)) { Write-IemCutoverFile -Path $p -Bytes $files[$p] -UserSid $u.sid }
    $back = Read-IemAutostartList -Paths $paths -UserSid $u.sid
    if ((@($back.tasks) -join '|') -cne (@($Tasks) -join '|') -or (@($back.run) -join '|') -cne (@($RunValues) -join '|') -or
        (@($back.startup) -join '|') -cne (@($Startup) -join '|')) { throw "$($paths.list) does not read back" }
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
                              tasks = @($Tasks); run = @($RunValues); startup = @($Startup); modules = [pscustomobject]$sums }
}

Export-ModuleMember -Function Install-IemCutover, Invoke-IemCutoverRequest, Invoke-IemCutoverVerb, Disable-IemAutostarts,
    Enable-IemAutostarts, Set-IemGuardLogon, Test-IemAutostartList, Compare-IemTaskXml, Read-IemAutostartExport
