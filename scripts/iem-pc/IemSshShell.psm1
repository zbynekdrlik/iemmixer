#Requires -Version 5.1
# The admin-only OpenSSH default shell (#15, the last item of the elevated
# chain; the design on #15 of 2026-10-08). With no
# HKLM\SOFTWARE\OpenSSH\DefaultShell, sshd runs every command as
# `cmd.exe /c`, and cmd without /d first runs the desktop user's
# HKCU\Software\Microsoft\Command Processor\AutoRun: a value the user may write
# would run inside every elevated ssh session. Three REG_SZ values close it:
# DefaultShell, the PC's own System32\cmd.exe as a literal path (the System
# known folder, never an environment variable; sshd treats a shell whose
# lower-cased path holds `system32\cmd` as cmd and builds the line as text,
# `"<shell>" <option> "<command>"`, so nothing iempc sends changes),
# DefaultShellCommandOption `/d /c`, and DefaultShellArguments `/d` (a session
# without a command). Every ssh connection runs an sshd process of its own,
# which reads the key: a change counts from the next session on.
#
# A wrong value could cut ssh to the PC, so Set-IemSshShell first saves the
# prior values in <elevated root>\ssh-shell\prior.json and arms a one-shot
# SYSTEM task (iemmixer-ssh-shell-undo, now + 10 min) that restores them
# (Undo-IemSshShell), and only then writes; `iempc ssh-shell` then probes a
# fresh session and only after that runs Confirm-IemSshShell, which removes
# the task and the saved values. Run elevated over ssh from the dev box,
# imported from the admin-only stage (iempc_sshshell.py). This module imports
# IemPc.psm1 from its own folder (the elevated root, file and task helpers), so
# IemPc.psm1 goes wherever it goes: the stage first, and the undo task's folder.
# Nothing is ended by force (I8); no site value lives here (P6): the key, the
# task folder and the elevated root are parameters (the defaults are the
# product's key, our task folder and the known folder).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'IemPc.psm1') -Force -Global

$script:ModuleFile = $PSCommandPath
$script:PcModuleFile = Join-Path $PSScriptRoot 'IemPc.psm1'
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding $false
$script:DefaultKey = 'HKLM:\SOFTWARE\OpenSSH'
$script:Names = @('DefaultShell', 'DefaultShellCommandOption', 'DefaultShellArguments')
$script:CommandOption = '/d /c'
$script:ShellArguments = '/d'
$script:DefaultTaskFolder = '\iemmixer'
$script:DefaultTaskName = 'iemmixer-ssh-shell-undo'
$script:UndoMinutes = 10
# <elevated root>\ssh-shell: the saved values, the undo task's entry script and
# its copies of this module and IemPc.psm1, and the task's log.
$script:StateDirName = 'ssh-shell'
$script:PriorName = 'prior.json'
$script:EntryName = 'ssh-shell-undo.ps1'
$script:LogName = 'undo.log'
$script:PriorVersion = 1
$script:SidAdmins = 'S-1-5-32-544'
$script:SidSystem = 'S-1-5-18'
$script:SidTrustedInstaller = 'S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464'
$script:KeyTrusted = @($script:SidAdmins, $script:SidSystem, $script:SidTrustedInstaller)
# Registry rights that change a key's values or its rules: set value (2),
# create subkey (4) and link (0x20), delete (0x10000), write DAC (0x40000) and
# owner (0x80000), generic all (0x10000000) and generic write (0x40000000).
# KEY_WRITE and full control hold set value.
$script:KeyChange = [int64]0x500D0026
# Task Scheduler: TASK_LOGON_SERVICE_ACCOUNT, TASK_TRIGGER_TIME,
# TASK_STATE_RUNNING, TASK_RUNLEVEL_HIGHEST, TASK_CREATE_OR_UPDATE with
# TASK_DONT_ADD_PRINCIPAL_ACE (as Register-IemTasks), and the undo task's
# descriptor: Administrators and SYSTEM only (the user neither runs nor reads it).
$script:LogonServiceAccount = 5
$script:TriggerTime = 1
$script:TaskRunning = 4
$script:RunLevelHighest = 1
$script:TaskDontAddPrincipalAce = 0x10
$script:TaskCreateOrUpdate = 6 -bor $script:TaskDontAddPrincipalAce
$script:TaskSddl = 'D:(A;;FA;;;BA)(A;;FA;;;SY)'

# The undo task's entry, written by Set-IemSshShell next to its copies of this
# module and IemPc.psm1. It never reads an environment variable for a path.
$script:UndoEntry = @'
# iemmixer #15: the one-shot undo of the admin-only OpenSSH default shell,
# written by Set-IemSshShell into <elevated root>\ssh-shell next to its copies
# of IemSshShell.psm1 and IemPc.psm1 (owner Administrators; only
# Administrators and SYSTEM may change it). The SYSTEM task
# iemmixer-ssh-shell-undo runs it unless `iempc ssh-shell` confirmed the new
# shell; it appends its result to undo.log here.
param([Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][string]$TaskFolder,
      [Parameter(Mandatory)][string]$TaskName, [Parameter(Mandatory)][string]$UserSid)
$env:PSModulePath = [IO.Path]::Combine($PSHOME, 'Modules') + ';' + [IO.Path]::Combine([Environment]::GetFolderPath('ProgramFiles'), 'WindowsPowerShell\Modules')
$ErrorActionPreference = 'Stop'
$code = 2
try {
    Import-Module ([IO.Path]::Combine($PSScriptRoot, 'IemSshShell.psm1')) -Force
    $r = Undo-IemSshShell -Key $Key -TaskFolder $TaskFolder -TaskName $TaskName -ElevatedRoot ([IO.Path]::GetDirectoryName($PSScriptRoot)) -UserSid $UserSid
    $line = 'undo ' + $r.state
    $code = 0
} catch {
    $line = 'undo failed: ' + ($_.Exception.Message -replace '[\r\n]+', ' ')
}
try {
    [IO.File]::AppendAllText([IO.Path]::Combine($PSScriptRoot, 'undo.log'), ([DateTime]::UtcNow.ToString('o') + ' ' + $line + "`r`n"))
} catch {
    [Console]::Error.WriteLine('iem-ssh-shell-undo: ' + $line + ' (undo.log not written: ' + $_.Exception.Message + ')')
}
exit $code
'@

# ---- the key's values, exactly ----

function Open-IemShellKey {
    # The key (-Key: HKLM:\...) in the 64-bit view sshd reads; $null when it
    # does not exist. -Write opens it for writing and creates a missing one.
    param([Parameter(Mandatory)][string]$Key, [switch]$Write)
    $m = [regex]::Match($Key, '^HKLM:\\(.*[^\\])$')
    if (-not $m.Success) { throw "key $Key refused: an HKLM:\... key" }
    $sub = $m.Groups[1].Value
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    if (-not $Write) { return $base.OpenSubKey($sub, $false) }
    $k = $base.OpenSubKey($sub, $true)
    if ($null -eq $k) { $k = $base.CreateSubKey($sub) }
    return $k
}

function ConvertTo-IemSavedValue {
    # One registry value as prior.json keeps it: its kind and its data, exact
    # (a string unexpanded, a QWORD as text, binary as base64). A kind that
    # could not be written back exactly is refused.
    param([Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][Microsoft.Win32.RegistryValueKind]$Kind, [AllowNull()]$Data)
    switch ("$Kind") {
        'String' { return [pscustomobject]@{ kind = 'String'; data = [string]$Data } }
        'ExpandString' { return [pscustomobject]@{ kind = 'ExpandString'; data = [string]$Data } }
        'MultiString' { return [pscustomobject]@{ kind = 'MultiString'; data = @([string[]]$Data) } }
        'DWord' { return [pscustomobject]@{ kind = 'DWord'; data = [int64][int]$Data } }
        'QWord' { return [pscustomobject]@{ kind = 'QWord'; data = ([int64]$Data).ToString([Globalization.CultureInfo]::InvariantCulture) } }
        'Binary' { return [pscustomobject]@{ kind = 'Binary'; data = [Convert]::ToBase64String([byte[]]$Data) } }
    }
    throw "$Name is a $Kind value, which cannot be saved and written back exactly: refused, nothing changed"
}

function Get-IemShellValues {
    # The three values as they are: each $null (absent) or its kind and data.
    param([Parameter(Mandatory)][string]$Key)
    $out = [ordered]@{}
    $k = Open-IemShellKey -Key $Key
    try {
        $present = @()
        if ($null -ne $k) { $present = @($k.GetValueNames()) }
        foreach ($n in $script:Names) {
            $out[$n] = $null
            if ($present -notcontains $n) { continue }
            $data = $k.GetValue($n, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $out[$n] = ConvertTo-IemSavedValue -Name $n -Kind $k.GetValueKind($n) -Data $data
        }
    } finally { if ($null -ne $k) { $k.Close() } }
    return [pscustomobject]$out
}

function Write-IemSavedValue {
    # One saved value written back as it was: absent deletes the value.
    param([Parameter(Mandatory)]$RegKey, [Parameter(Mandatory)][string]$Name, [AllowNull()]$Saved)
    if ($null -eq $Saved) {
        $RegKey.DeleteValue($Name, $false)
        return
    }
    $kind = [string](Get-IemProp $Saved 'kind')
    $data = Get-IemProp $Saved 'data'
    switch ($kind) {
        'String' { $RegKey.SetValue($Name, [string]$data, [Microsoft.Win32.RegistryValueKind]::String); return }
        'ExpandString' { $RegKey.SetValue($Name, [string]$data, [Microsoft.Win32.RegistryValueKind]::ExpandString); return }
        'MultiString' { $RegKey.SetValue($Name, [string[]]@($data), [Microsoft.Win32.RegistryValueKind]::MultiString); return }
        'DWord' { $RegKey.SetValue($Name, [int]$data, [Microsoft.Win32.RegistryValueKind]::DWord); return }
        'QWord' { $RegKey.SetValue($Name, [int64]::Parse([string]$data, [Globalization.CultureInfo]::InvariantCulture), [Microsoft.Win32.RegistryValueKind]::QWord); return }
        'Binary' { $RegKey.SetValue($Name, [Convert]::FromBase64String([string]$data), [Microsoft.Win32.RegistryValueKind]::Binary); return }
    }
    throw "${Name}: a saved kind '$kind' is not known: refused"
}

function Format-IemSavedValue {
    # One value as text, to compare exactly: `absent`, or its kind and data.
    param([AllowNull()]$Saved)
    if ($null -eq $Saved) { return 'absent' }
    $kind = [string](Get-IemProp $Saved 'kind')
    $data = Get-IemProp $Saved 'data'
    if ($kind -ceq 'MultiString') {
        $items = @(@($data) | ForEach-Object { [string]$_ })
        return ('MultiString:{0}:{1}' -f $items.Count, ($items -join [char]0))
    }
    return ('{0}:{1}' -f $kind, [string]$data)
}

function Test-IemSameShellValues {
    param($A, $B)
    foreach ($n in $script:Names) {
        if ((Format-IemSavedValue (Get-IemProp $A $n)) -cne (Format-IemSavedValue (Get-IemProp $B $n))) { return $false }
    }
    return $true
}

function Get-IemShellCmd {
    # The PC's own cmd.exe, from the System known folder (never an environment variable).
    $system = [Environment]::GetFolderPath('System')
    if (-not $system) { throw 'the System known folder is unknown' }
    $cmd = Join-Path $system 'cmd.exe'
    if (-not (Test-Path -LiteralPath $cmd -PathType Leaf)) { throw "$cmd does not exist" }
    # sshd's own test for a cmd shell (Win32-OpenSSH): the lower-cased path holds system32\cmd.
    if (-not $cmd.ToLowerInvariant().Contains('system32\cmd')) { throw "$cmd is not one sshd takes for cmd (system32\cmd)" }
    return $cmd
}

function Get-IemOurShellValues {
    return [pscustomobject][ordered]@{
        DefaultShell = [pscustomobject]@{ kind = 'String'; data = (Get-IemShellCmd) }
        DefaultShellCommandOption = [pscustomobject]@{ kind = 'String'; data = $script:CommandOption }
        DefaultShellArguments = [pscustomobject]@{ kind = 'String'; data = $script:ShellArguments }
    }
}

function Test-IemShellKeyAcl {
    # The key's rules read back (the design: no right to change it for anyone
    # but Administrators, SYSTEM and TrustedInstaller): owned by one of them,
    # and no allow rule that applies to the key itself (an inherit-only rule
    # reaches only subkeys) gives anyone else a right in KeyChange. Returns the
    # differences. -Missing: a key that does not exist yet is no difference
    # (Set creates it, then reads it back).
    param([Parameter(Mandatory)][string]$Key, [switch]$Missing)
    $k = Open-IemShellKey -Key $Key
    if ($null -eq $k) {
        if ($Missing) { return ,@() }
        return ,@("$Key does not exist")
    }
    try { $acl = $k.GetAccessControl() } finally { $k.Close() }
    $bad = @()
    $owner = $acl.GetOwner([System.Security.Principal.SecurityIdentifier]).Value
    if ($script:KeyTrusted -notcontains $owner) { $bad += "$Key is owned by $owner" }
    foreach ($r in @($acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))) {
        if ("$($r.AccessControlType)" -ne 'Allow') { continue }
        if (([int]$r.PropagationFlags -band [int][System.Security.AccessControl.PropagationFlags]::InheritOnly) -ne 0) { continue }
        $sid = $r.IdentityReference.Value
        if ($script:KeyTrusted -contains $sid) { continue }
        if (([int64][int]$r.RegistryRights -band $script:KeyChange) -ne 0) { $bad += ('{0} may be changed by {1} ({2})' -f $Key, $sid, $r.RegistryRights) }
    }
    return ,$bad
}

# ---- the saved values and the undo task's files ----

function Get-IemShellPaths {
    param([string]$ElevatedRoot = '')
    $root = Resolve-IemElevatedRoot -ElevatedRoot $ElevatedRoot
    $dir = Join-Path $root $script:StateDirName
    return [pscustomobject]@{ root = $root; dir = $dir; prior = (Join-Path $dir $script:PriorName)
                              entry = (Join-Path $dir $script:EntryName); log = (Join-Path $dir $script:LogName) }
}

function Read-IemSavedShell {
    # prior.json, $null when there is none. The elevated root, the folder and
    # the file must read back admin-only (Test-IemElevatedItem: no restore
    # from values someone else could have written), the file must be this
    # version, for this key, with the three values in a form Undo writes back.
    param([Parameter(Mandatory)]$Paths, [Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][string]$UserSid)
    if (Test-IemReparsePoint -Path $Paths.prior) { throw "the saved values are refused: $($Paths.prior) is a junction or a link (inspect it by hand)" }
    if (-not (Test-Path -LiteralPath $Paths.prior)) { return $null }
    $bad = @()
    foreach ($p in @($Paths.root, $Paths.dir, $Paths.prior)) { $bad += Test-IemElevatedItem -Path $p -UserSid $UserSid }
    if ($bad.Count -gt 0) { throw ('the saved values are refused (inspect them by hand): ' + ($bad -join '; ')) }
    $doc = [IO.File]::ReadAllText($Paths.prior, $script:Utf8NoBom) | ConvertFrom-Json
    if ((Get-IemProp $doc 'version') -ne $script:PriorVersion) { throw "$($Paths.prior): version $(Get-IemProp $doc 'version'), not $($script:PriorVersion): refused" }
    if ([string](Get-IemProp $doc 'key') -cne $Key) { throw "$($Paths.prior) saves $(Get-IemProp $doc 'key'), not ${Key}: refused" }
    $values = Get-IemProp $doc 'values'
    if ($null -eq $values) { throw "$($Paths.prior) holds no values: refused" }
    foreach ($n in $script:Names) {
        if ($null -eq $values.PSObject.Properties[$n]) { throw "$($Paths.prior) does not name ${n}: refused" }
        $v = $values.$n
        if ($null -ne $v -and (@('String', 'ExpandString', 'MultiString', 'DWord', 'QWord', 'Binary') -cnotcontains [string](Get-IemProp $v 'kind') -or
                               $null -eq $v.PSObject.Properties['data'])) {
            throw "$($Paths.prior): $n is no value Undo can write back: refused"
        }
    }
    return $doc
}

function Write-IemSavedShell {
    # The values as they were before any change of ours, written fresh and
    # read back (admin-only, the same values).
    param([Parameter(Mandatory)]$Paths, [Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)]$Values, [Parameter(Mandatory)][string]$UserSid)
    $doc = [pscustomobject][ordered]@{ version = $script:PriorVersion; key = $Key; saved_at = [DateTime]::UtcNow.ToString('o'); values = $Values }
    Write-IemElevatedFile -Path $Paths.prior -Bytes $script:Utf8NoBom.GetBytes((ConvertTo-Json -InputObject $doc -Depth 6))
    $back = Read-IemSavedShell -Paths $Paths -Key $Key -UserSid $UserSid
    if ($null -eq $back -or -not (Test-IemSameShellValues (Get-IemProp $back 'values') $Values)) { throw "$($Paths.prior) does not read back" }
    return $back
}

function Install-IemShellUndoFiles {
    # The undo task's folder: the entry script and copies of this module and
    # its IemPc.psm1 (the stage copies the dev box checked), admin-only and read back.
    param([Parameter(Mandatory)]$Paths, [Parameter(Mandatory)][string]$UserSid)
    Install-IemElevatedFolder -Path $Paths.dir -UserSid $UserSid
    $files = [ordered]@{}
    $files[$Paths.entry] = $script:Utf8NoBom.GetBytes($script:UndoEntry)
    $files[(Join-Path $Paths.dir 'IemSshShell.psm1')] = [IO.File]::ReadAllBytes($script:ModuleFile)
    $files[(Join-Path $Paths.dir 'IemPc.psm1')] = [IO.File]::ReadAllBytes($script:PcModuleFile)
    foreach ($p in @($files.Keys)) {
        # A module loaded from this folder itself is never rewritten under its own import.
        if ([string]::Equals($p, $script:ModuleFile, [StringComparison]::OrdinalIgnoreCase) -or
            [string]::Equals($p, $script:PcModuleFile, [StringComparison]::OrdinalIgnoreCase)) { continue }
        Write-IemElevatedFile -Path $p -Bytes $files[$p]
    }
    foreach ($p in @($files.Keys)) {
        $bad = Test-IemElevatedItem -Path $p -UserSid $UserSid
        if ($bad.Count -gt 0) { throw ('undo file read-back: ' + ($bad -join '; ')) }
        if ((Get-IemBytesSha256 -Bytes ([IO.File]::ReadAllBytes($p))) -cne (Get-IemBytesSha256 -Bytes $files[$p])) { throw "$p does not read back" }
    }
}

function Get-IemLastUndo {
    # The undo task's last line in undo.log, or $null.
    param([Parameter(Mandatory)]$Paths)
    if (-not (Test-Path -LiteralPath $Paths.log -PathType Leaf) -or (Test-IemReparsePoint -Path $Paths.log)) { return $null }
    $lines = @([IO.File]::ReadAllLines($Paths.log) | Where-Object { $_.Trim() })
    if ($lines.Count -eq 0) { return $null }
    return $lines[-1]
}

# ---- the undo task ----

function Test-IemUndoTaskSddl {
    # The undo task's DACL read back: its own rules exactly Administrators and
    # SYSTEM with full rights (the generic or the file form); rules inherited
    # from the task folder only for them; nothing else, no deny, no object rule.
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Sddl)
    try { $sd = New-Object System.Security.AccessControl.RawSecurityDescriptor $Sddl } catch { return $false }
    if ($null -eq $sd.DiscretionaryAcl) { return $false }
    $seen = @{}
    foreach ($ace in $sd.DiscretionaryAcl) {
        if ($ace -isnot [System.Security.AccessControl.CommonAce]) { return $false }
        if ($ace.AceQualifier -ne [System.Security.AccessControl.AceQualifier]::AccessAllowed) { return $false }
        $sid = $ace.SecurityIdentifier.Value
        if (@($script:SidAdmins, $script:SidSystem) -cnotcontains $sid) { return $false }
        if ($ace.IsInherited) { continue }
        if ($seen.ContainsKey($sid)) { return $false }
        $mask = ([int64]$ace.AccessMask) -band [int64]4294967295
        if (@([int64]268435456, [int64]2032127) -notcontains $mask) { return $false }
        $seen[$sid] = $true
    }
    return ($seen.Count -eq 2)
}

function ConvertTo-IemSid {
    # A task principal's UserId (a name or a SID) as a SID.
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Name)
    if ($Name -cmatch '^S-1-[0-9-]+$') { return $Name }
    try { return (New-Object System.Security.Principal.NTAccount $Name).Translate([System.Security.Principal.SecurityIdentifier]).Value } catch { return "unknown ($Name)" }
}

function Get-IemUndoTask {
    # The registered undo task, or $null.
    param([Parameter(Mandatory)][string]$TaskFolder, [Parameter(Mandatory)][string]$TaskName)
    return (Get-IemRegisteredTask -Scheduler (Connect-IemScheduler) -Folder $TaskFolder -Name $TaskName)
}

function Remove-IemUndoTask {
    # Removes the registration (a running instance runs on: never ended); $true when there was one.
    param([Parameter(Mandatory)][string]$TaskFolder, [Parameter(Mandatory)][string]$TaskName)
    $f = Get-IemTaskFolder -Scheduler (Connect-IemScheduler) -Path $TaskFolder
    if ($null -eq $f) { return $false }
    try { [void]$f.GetTask($TaskName) } catch { return $false }
    $f.DeleteTask($TaskName, 0)
    return $true
}

function Register-IemUndoTask {
    # The one-shot undo: a SYSTEM task in -TaskFolder that runs the entry
    # script once at now + -Minutes, and after a missed start as soon as it
    # can (StartWhenAvailable: a reboot in between); only Administrators and
    # SYSTEM may read, run or change it; never ended hard. Read back; returns
    # its start time.
    param([Parameter(Mandatory)]$Paths, [Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][string]$TaskFolder,
          [Parameter(Mandatory)][string]$TaskName, [Parameter(Mandatory)][string]$UserSid, [Parameter(Mandatory)][int]$Minutes)
    $sch = Connect-IemScheduler
    $f = Get-IemTaskFolder -Scheduler $sch -Path $TaskFolder
    if ($null -eq $f) { $f = $sch.GetFolder('\').CreateFolder($TaskFolder.Trim('\')) }
    $system = [Environment]::GetFolderPath('System')
    $ps = Join-Path $system 'WindowsPowerShell\v1.0\powershell.exe'
    $taskArgs = '-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File ' + (Format-IemArg $Paths.entry) +
        ' -Key ' + (Format-IemArg $Key) + ' -TaskFolder ' + (Format-IemArg $TaskFolder) + ' -TaskName ' + (Format-IemArg $TaskName) +
        ' -UserSid ' + (Format-IemArg $UserSid)
    $at = [DateTime]::Now.AddMinutes($Minutes).ToString('s', [Globalization.CultureInfo]::InvariantCulture)
    $d = $sch.NewTask(0)
    $d.RegistrationInfo.Description = 'iemmixer #15: restores the prior OpenSSH default shell unless iempc ssh-shell confirmed the new one'
    $d.Principal.LogonType = $script:LogonServiceAccount
    $d.Principal.RunLevel = $script:RunLevelHighest
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
    $s.AllowDemandStart = $true
    $s.StartWhenAvailable = $true
    $a = $d.Actions.Create(0)
    $a.Path = $ps
    $a.Arguments = $taskArgs
    $a.WorkingDirectory = $Paths.dir
    $t = $d.Triggers.Create($script:TriggerTime)
    $t.StartBoundary = $at
    $t.Enabled = $true
    # SYSTEM by its SID's name (localized on some Windows languages).
    $account = (New-Object System.Security.Principal.SecurityIdentifier $script:SidSystem).Translate([System.Security.Principal.NTAccount]).Value
    [void]$f.RegisterTaskDefinition($TaskName, $d, $script:TaskCreateOrUpdate, $account, $null, $script:LogonServiceAccount, $script:TaskSddl)
    [void]$f.GetTask($TaskName).SetSecurityDescriptor($script:TaskSddl, $script:TaskDontAddPrincipalAce)
    $task = $f.GetTask($TaskName)
    $back = $task.Definition
    $bad = @()
    $uid = ConvertTo-IemSid -Name ([string]$back.Principal.UserId)
    if ($uid -cne $script:SidSystem) { $bad += "runs as $uid" }
    if ([int]$back.Principal.LogonType -ne $script:LogonServiceAccount) { $bad += "logon type $($back.Principal.LogonType)" }
    if ([int]$back.Principal.RunLevel -ne $script:RunLevelHighest) { $bad += "run level $($back.Principal.RunLevel)" }
    $acts = Get-IemTaskActions -Definition $back
    if (-not (Test-IemSameActions -A @([pscustomobject]@{ path = $ps; arguments = $taskArgs; workdir = $Paths.dir }) -B $acts)) {
        $bad += ('action ' + (ConvertTo-Json -InputObject $acts -Compress))
    }
    $trs = @($back.Triggers)
    if ($trs.Count -ne 1 -or [int]$trs[0].Type -ne $script:TriggerTime -or -not [bool]$trs[0].Enabled -or [datetime]$trs[0].StartBoundary -ne [datetime]$at) {
        $bad += ('triggers ' + (@($trs | ForEach-Object { '{0}@{1}' -f $_.Type, $_.StartBoundary }) -join ','))
    }
    $bs = $back.Settings
    if (-not $bs.Enabled -or $bs.AllowHardTerminate -or -not $bs.StartWhenAvailable -or [string]$bs.ExecutionTimeLimit -cne 'PT0S') { $bad += 'settings' }
    $sddl = [string]$task.GetSecurityDescriptor(4)
    if (-not (Test-IemUndoTaskSddl -Sddl $sddl)) { $bad += "security descriptor $sddl" }
    if ($bad.Count -gt 0) { throw ("undo task read-back ($TaskFolder\$TaskName): " + ($bad -join '; ')) }
    return $at
}

# ---- the three functions iempc runs, and the read-only state ----

function Get-IemSshShell {
    # The key's three values, whether they are ours, and what is armed: the
    # saved values, the undo task (its state and next run) and its last log line.
    param([string]$Key = $script:DefaultKey, [string]$TaskFolder = $script:DefaultTaskFolder, [string]$TaskName = $script:DefaultTaskName,
          [string]$ElevatedRoot = '')
    $paths = Get-IemShellPaths -ElevatedRoot $ElevatedRoot
    $values = Get-IemShellValues -Key $Key
    $task = Get-IemUndoTask -TaskFolder $TaskFolder -TaskName $TaskName
    $undo = $null
    if ($null -ne $task) { $undo = [pscustomobject]@{ state = [int]$task.State; next = [string]$task.NextRunTime } }
    [pscustomobject]@{ key = $Key; values = $values; ours = (Test-IemSameShellValues $values (Get-IemOurShellValues))
                       saved = (Test-Path -LiteralPath $paths.prior); undo = $undo; undo_log = (Get-IemLastUndo -Paths $paths) }
}

function Set-IemSshShell {
    # Idempotent. Values already ours with nothing saved: `unchanged` (an undo
    # task without saved values could restore nothing and is removed).
    # Otherwise the values as they are now are saved (`set`), or a pending
    # save is kept, since it holds the values from before our first change
    # (`rearmed`); the undo task is armed (or moved to now + -UndoMinutes);
    # only then are the three values written. Read back: each a REG_SZ with
    # our data, DefaultShell an existing file equal to System32\cmd.exe, and
    # the key's rules (Test-IemShellKeyAcl). Refused before anything changes:
    # an elevated root that is not admin-only, a key someone else may change,
    # a value that could not be written back exactly, saved values that do not
    # read back admin-only, an undo task that runs now. A failure after the
    # undo task was armed names it: it restores the prior values at its time.
    param([string]$Key = $script:DefaultKey, [string]$TaskFolder = $script:DefaultTaskFolder, [string]$TaskName = $script:DefaultTaskName,
          [string]$ElevatedRoot = '', [string]$User = '', [ValidateRange(2, 60)][int]$UndoMinutes = $script:UndoMinutes)
    $u = Resolve-IemUser -User $User
    $paths = Get-IemShellPaths -ElevatedRoot $ElevatedRoot
    $rootBad = Test-IemElevatedItem -Path $paths.root -UserSid $u.sid
    if ($rootBad.Count -gt 0) { throw ('the elevated root is refused (Register-IemTasks makes it): ' + ($rootBad -join '; ')) }
    $ours = Get-IemOurShellValues
    # The key's rules first, then its values: a key someone else may change is refused whatever it holds.
    $keyBad = Test-IemShellKeyAcl -Key $Key -Missing
    if ($keyBad.Count -gt 0) { throw ('refused, nothing changed: ' + ($keyBad -join '; ')) }
    $current = Get-IemShellValues -Key $Key
    $saved = Read-IemSavedShell -Paths $paths -Key $Key -UserSid $u.sid
    $task = Get-IemUndoTask -TaskFolder $TaskFolder -TaskName $TaskName
    if ($null -ne $task -and [int]$task.State -eq $script:TaskRunning) {
        throw "the undo task $TaskFolder\$TaskName runs now: nothing changed; run this again once it has ended"
    }
    if ($null -eq $saved -and (Test-IemSameShellValues $current $ours)) {
        if ($null -ne $task) { [void](Remove-IemUndoTask -TaskFolder $TaskFolder -TaskName $TaskName) }
        return [pscustomobject]@{ state = 'unchanged'; key = $Key; values = $current; prior = $null; undo = $null; undo_log = (Get-IemLastUndo -Paths $paths) }
    }
    $state = 'rearmed'
    Install-IemShellUndoFiles -Paths $paths -UserSid $u.sid
    if ($null -eq $saved) {
        $state = 'set'
        $saved = Write-IemSavedShell -Paths $paths -Key $Key -Values $current -UserSid $u.sid
    }
    $at = Register-IemUndoTask -Paths $paths -Key $Key -TaskFolder $TaskFolder -TaskName $TaskName -UserSid $u.sid -Minutes $UndoMinutes
    try {
        $k = Open-IemShellKey -Key $Key -Write
        try {
            foreach ($n in $script:Names) { Write-IemSavedValue -RegKey $k -Name $n -Saved (Get-IemProp $ours $n) }
        } finally { $k.Close() }
        $read = Get-IemShellValues -Key $Key
        if (-not (Test-IemSameShellValues $read $ours)) {
            throw ('the values do not read back: ' + (@($script:Names | ForEach-Object { '{0}={1}' -f $_, (Format-IemSavedValue (Get-IemProp $read $_)) }) -join '; '))
        }
        $keyBad = Test-IemShellKeyAcl -Key $Key
        if ($keyBad.Count -gt 0) { throw ('the key reads back: ' + ($keyBad -join '; ')) }
    } catch {
        throw ("$_" + "; the undo task $TaskFolder\$TaskName stays armed: it restores the prior values at $at")
    }
    [pscustomobject]@{ state = $state; key = $Key; values = $read; prior = (Get-IemProp $saved 'values')
                       undo = [pscustomobject]@{ task = ($TaskFolder.TrimEnd('\') + '\' + $TaskName); at = $at }
                       undo_log = (Get-IemLastUndo -Paths $paths) }
}

function Confirm-IemSshShell {
    # After a fresh session ran with /d: removes the undo task (first, so it
    # cannot fire after this), then the saved values. Only when the three
    # values are ours and the key reads back admin-only; never while the undo
    # task runs. `confirmed`, or `unchanged` when nothing was armed.
    param([string]$Key = $script:DefaultKey, [string]$TaskFolder = $script:DefaultTaskFolder, [string]$TaskName = $script:DefaultTaskName,
          [string]$ElevatedRoot = '')
    $paths = Get-IemShellPaths -ElevatedRoot $ElevatedRoot
    $current = Get-IemShellValues -Key $Key
    if (-not (Test-IemSameShellValues $current (Get-IemOurShellValues))) {
        throw ('the values are not ours (' + (@($script:Names | ForEach-Object { '{0}={1}' -f $_, (Format-IemSavedValue (Get-IemProp $current $_)) }) -join '; ') +
               '): nothing confirmed; an armed undo task stays')
    }
    $keyBad = Test-IemShellKeyAcl -Key $Key
    if ($keyBad.Count -gt 0) { throw ('nothing confirmed, an armed undo task stays: ' + ($keyBad -join '; ')) }
    $task = Get-IemUndoTask -TaskFolder $TaskFolder -TaskName $TaskName
    if ($null -ne $task -and [int]$task.State -eq $script:TaskRunning) {
        throw "the undo task $TaskFolder\$TaskName runs now: nothing confirmed; run iempc ssh-shell again once it has ended"
    }
    $removed = @()
    if (Remove-IemUndoTask -TaskFolder $TaskFolder -TaskName $TaskName) { $removed += 'undo task' }
    if (Test-IemReparsePoint -Path $paths.prior) { throw "$($paths.prior) is a junction or a link: refused" }
    if (Test-Path -LiteralPath $paths.prior) {
        [IO.File]::Delete($paths.prior)
        $removed += 'saved values'
    }
    $state = 'unchanged'
    if ($removed.Count -gt 0) { $state = 'confirmed' }
    [pscustomobject]@{ state = $state; removed = $removed; key = $Key }
}

function Undo-IemSshShell {
    # What the undo task runs (also by hand): the saved values written back
    # exactly (absent deletes the value) and read back, then the saved values
    # and the task removed. Only from saved values that read back admin-only
    # (-UserSid: the user in the elevated folders' rules, which the task
    # passes, since it runs as SYSTEM). `restored`, or `nothing-saved` (a
    # leftover task is removed).
    param([string]$Key = $script:DefaultKey, [string]$TaskFolder = $script:DefaultTaskFolder, [string]$TaskName = $script:DefaultTaskName,
          [string]$ElevatedRoot = '', [string]$User = '', [string]$UserSid = '')
    $sid = $UserSid
    if (-not $sid) { $sid = (Resolve-IemUser -User $User).sid }
    $paths = Get-IemShellPaths -ElevatedRoot $ElevatedRoot
    $saved = Read-IemSavedShell -Paths $paths -Key $Key -UserSid $sid
    if ($null -eq $saved) {
        [void](Remove-IemUndoTask -TaskFolder $TaskFolder -TaskName $TaskName)
        return [pscustomobject]@{ state = 'nothing-saved'; key = $Key; values = (Get-IemShellValues -Key $Key) }
    }
    $prior = Get-IemProp $saved 'values'
    $k = Open-IemShellKey -Key $Key -Write
    try {
        foreach ($n in $script:Names) { Write-IemSavedValue -RegKey $k -Name $n -Saved (Get-IemProp $prior $n) }
    } finally { $k.Close() }
    $read = Get-IemShellValues -Key $Key
    if (-not (Test-IemSameShellValues $read $prior)) {
        throw ('the restored values do not read back: ' + (@($script:Names | ForEach-Object { '{0}={1}' -f $_, (Format-IemSavedValue (Get-IemProp $read $_)) }) -join '; '))
    }
    [IO.File]::Delete($paths.prior)
    try { [void](Remove-IemUndoTask -TaskFolder $TaskFolder -TaskName $TaskName) } catch {
        throw "the prior values are restored; the undo task $TaskFolder\$TaskName was not removed: $($_.Exception.Message)"
    }
    [pscustomobject]@{ state = 'restored'; key = $Key; values = $read }
}

Export-ModuleMember -Function Get-IemSshShell, Set-IemSshShell, Confirm-IemSshShell, Undo-IemSshShell, Test-IemUndoTaskSddl
