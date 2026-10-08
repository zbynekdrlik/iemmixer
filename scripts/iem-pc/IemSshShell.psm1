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
# the saved values and the task. Run elevated over ssh from the dev box,
# imported from the admin-only stage (iempc_sshshell.py). This module imports
# two modules from its own folder, so they go wherever it goes (the stage, and
# the undo task's folder): IemPc.psm1 (the elevated root, file and task
# helpers) and S1c's IemTuningStore.psm1, whose Get-IemRegRaw, Set-IemRegRaw
# and Test-IemRegRawSame save one registry value and write it back exactly
# (absent deletes it). Nothing is ended by force (I8); no site value lives
# here (P6): the key, the task folder and the elevated root are parameters
# (the defaults are the product's key, our task folder and the known folder).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'IemPc.psm1') -Force -Global
Import-Module (Join-Path $PSScriptRoot 'IemTuningStore.psm1') -Force -Global

$script:ModuleFile = $PSCommandPath
# The modules the undo task's folder carries: this one and the two it imports.
$script:ModuleFiles = @($PSCommandPath, (Join-Path $PSScriptRoot 'IemPc.psm1'), (Join-Path $PSScriptRoot 'IemTuningStore.psm1'))
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding $false
$script:DefaultKey = 'HKLM:\SOFTWARE\OpenSSH'
$script:Names = @('DefaultShell', 'DefaultShellCommandOption', 'DefaultShellArguments')
$script:CommandOption = '/d /c'
$script:ShellArguments = '/d'
$script:DefaultTaskFolder = '\iemmixer'
$script:DefaultTaskName = 'iemmixer-ssh-shell-undo'
$script:UndoMinutes = 10
# <elevated root>\ssh-shell: the saved values, the undo task's entry script and
# its module copies, and the task's log.
$script:StateDirName = 'ssh-shell'
$script:PriorName = 'prior.json'
$script:EntryName = 'ssh-shell-undo.ps1'
$script:LogName = 'undo.log'
$script:PriorVersion = 1
# The kinds Get-IemRegRaw saves (anything else it refuses).
$script:Kinds = @('absent', 'String', 'ExpandString', 'MultiString', 'DWord', 'QWord', 'Binary')
# Set, Confirm and Undo (the SYSTEM task included) never overlap: one Global\
# lock, as IemTuningStore's boot lock (no DACL of its own: the .NET Framework
# constructor opens an existing one with MUTEX_MODIFY_STATE | SYNCHRONIZE, which
# every elevated caller and SYSTEM get from the creator's default DACL).
$script:LockName = 'Global\iemmixer-ssh-shell'
$script:LockWaitMs = 60000
$script:SidAdmins = 'S-1-5-32-544'
$script:SidSystem = 'S-1-5-18'
$script:SidUsers = 'S-1-5-32-545'
$script:SidTrustedInstaller = 'S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464'
$script:KeyTrusted = @($script:SidAdmins, $script:SidSystem, $script:SidTrustedInstaller)
# Registry rights that change a key's values or its rules: set value (2),
# create subkey (4) and link (0x20), delete (0x10000), write DAC (0x40000) and
# owner (0x80000), generic all (0x10000000) and generic write (0x40000000).
# KEY_WRITE and full control hold set value.
$script:KeyChange = [int64]0x500D0026
# Task Scheduler: TASK_LOGON_SERVICE_ACCOUNT, TASK_TRIGGER_TIME, the states
# TASK_STATE_QUEUED and TASK_STATE_RUNNING, TASK_RUNLEVEL_HIGHEST,
# TASK_CREATE_OR_UPDATE with TASK_DONT_ADD_PRINCIPAL_ACE (as Register-IemTasks),
# and the undo task's descriptor: Administrators and SYSTEM only (the user
# neither runs nor reads it).
$script:LogonServiceAccount = 5
$script:TriggerTime = 1
$script:TaskBusy = @(2, 4)
$script:RunLevelHighest = 1
$script:TaskDontAddPrincipalAce = 0x10
$script:TaskCreateOrUpdate = 6 -bor $script:TaskDontAddPrincipalAce
$script:TaskSddl = 'D:(A;;FA;;;BA)(A;;FA;;;SY)'

# The undo task's entry, written by Set-IemSshShell next to its module copies.
# It never reads an environment variable for a path.
$script:UndoEntry = @'
# iemmixer #15: the one-shot undo of the admin-only OpenSSH default shell,
# written by Set-IemSshShell into <elevated root>\ssh-shell next to its copies
# of IemSshShell.psm1, IemPc.psm1 and IemTuningStore.psm1 (owner
# Administrators; only Administrators and SYSTEM may change it). The SYSTEM
# task iemmixer-ssh-shell-undo runs it unless `iempc ssh-shell` confirmed the
# new shell; it appends its result to undo.log here.
param([Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][string]$TaskFolder,
      [Parameter(Mandatory)][string]$TaskName, [Parameter(Mandatory)][string]$UserSid)
$env:PSModulePath = [IO.Path]::Combine($PSHOME, 'Modules') + ';' + [IO.Path]::Combine([Environment]::GetFolderPath('ProgramFiles'), 'WindowsPowerShell\Modules')
$ErrorActionPreference = 'Stop'
$code = 2
try {
    Import-Module ([IO.Path]::Combine($PSScriptRoot, 'IemSshShell.psm1')) -Force
    $r = Undo-IemSshShell -Key $Key -TaskFolder $TaskFolder -TaskName $TaskName -ElevatedRoot ([IO.Path]::GetDirectoryName($PSScriptRoot)) -UserSid $UserSid
    $line = 'undo ' + $r.state
    foreach ($p in @('why', 'lock')) {
        if ($null -ne $r.PSObject.Properties[$p] -and $r.$p) { $line += ': ' + ([string]$r.$p -replace '[\r\n]+', ' ') }
    }
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

# ---- the lock ----

function Invoke-IemShellLocked {
    # Runs -Body under the lock of Set, Confirm and Undo: waited for at most
    # LockWaitMs, an abandoned one (its holder ended without releasing it) taken
    # over, always released. Returns what -Body returns. A lock not free in
    # time refuses, except with -GoOn (the one-shot undo: any process may
    # create a Global\ name first, and the undo's job is to give ssh back):
    # -Body then runs without it and its result says so (`lock`).
    param([Parameter(Mandatory)][scriptblock]$Body, [switch]$GoOn)
    $lock = New-Object -TypeName System.Threading.Mutex -ArgumentList $false, $script:LockName
    $held = $false
    try {
        try { $held = $lock.WaitOne($script:LockWaitMs) }
        catch {
            if ($_.Exception.GetBaseException() -isnot [System.Threading.AbandonedMutexException]) { throw }
            $held = $true
        }
        $note = "the ssh-shell lock was not free within $($script:LockWaitMs / 1000) s (another Set, Confirm or Undo runs, or something else holds it)"
        if (-not $held -and -not $GoOn) { throw "${note}: nothing changed" }
        $r = & $Body
        if (-not $held -and $r -is [pscustomobject]) { $r | Add-Member -NotePropertyName lock -NotePropertyValue "$note; went on without it" }
        return $r
    } finally {
        if ($held) { $lock.ReleaseMutex() }
        $lock.Dispose()
    }
}

# ---- the key and its values ----

function Assert-IemShellKey {
    # An HKLM:\ key, read in the view sshd reads: a 32-bit PowerShell on a
    # 64-bit Windows would see HKLM\SOFTWARE's WOW64 copy instead.
    param([Parameter(Mandatory)][string]$Key)
    if ($Key -cnotmatch '^HKLM:\\.*[^\\]$') { throw "key $Key refused: an HKLM:\... key" }
    if ([Environment]::Is64BitOperatingSystem -and -not [Environment]::Is64BitProcess) {
        throw 'a 32-bit PowerShell reads the WOW64 view of HKLM\SOFTWARE, not the key sshd reads: refused'
    }
}

function Get-IemShellValues {
    # The three values as they are, each exactly as Get-IemRegRaw saves it
    # (kind 'absent', or the kind and the data); a kind it cannot write back is refused.
    param([Parameter(Mandatory)][string]$Key)
    $out = [ordered]@{}
    foreach ($n in $script:Names) { $out[$n] = Get-IemRegRaw -Path $Key -Name $n }
    return [pscustomobject]$out
}

function Test-IemSameShellValues {
    param([Parameter(Mandatory)]$A, [Parameter(Mandatory)]$B)
    foreach ($n in $script:Names) {
        if (-not (Test-IemRegRawSame -A (Get-IemProp $A $n) -B (Get-IemProp $B $n))) { return $false }
    }
    return $true
}

function Format-IemShellValues {
    # The three values for an error message.
    param([Parameter(Mandatory)]$Values)
    return (@($script:Names | ForEach-Object { '{0}={1}' -f $_, (ConvertTo-Json -InputObject (Get-IemProp $Values $_) -Compress) }) -join '; ')
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
        DefaultShell = @{ kind = 'String'; data = (Get-IemShellCmd) }
        DefaultShellCommandOption = @{ kind = 'String'; data = $script:CommandOption }
        DefaultShellArguments = @{ kind = 'String'; data = $script:ShellArguments }
    }
}

function New-IemShellKey {
    # A key that does not exist yet, created admin-only in one step (owner
    # Administrators; Administrators and SYSTEM full, Users read; nothing
    # inherited from HKLM\SOFTWARE) before any value goes into it.
    param([Parameter(Mandatory)][string]$Key)
    $sec = New-Object System.Security.AccessControl.RegistrySecurity
    $sec.SetOwner((New-Object System.Security.Principal.SecurityIdentifier $script:SidAdmins))
    $sec.SetAccessRuleProtection($true, $false)
    foreach ($a in @(@($script:SidAdmins, 'FullControl'), @($script:SidSystem, 'FullControl'), @($script:SidUsers, 'ReadKey'))) {
        $sec.AddAccessRule((New-Object System.Security.AccessControl.RegistryAccessRule((New-Object System.Security.Principal.SecurityIdentifier $a[0]),
            [System.Security.AccessControl.RegistryRights]$a[1], 'ContainerInherit', 'None', 'Allow')))
    }
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $k = $base.CreateSubKey($Key.Substring('HKLM:\'.Length), [Microsoft.Win32.RegistryKeyPermissionCheck]::ReadWriteSubTree, $sec)
        $k.Close()
    } finally { $base.Close() }
}

function Test-IemShellKeyAcl {
    # The key's security read back (the design: no right to change it for
    # anyone but Administrators, SYSTEM and TrustedInstaller): owned by one of
    # them, a DACL, and no allow entry that applies to the key itself (an
    # inherit-only one reaches only subkeys) giving anyone else a right in
    # KeyChange. The raw DACL is read, so a conditional (callback) entry counts
    # like any other; an entry of a type this cannot read is a difference.
    # Returns the differences. -Missing: a key that does not exist yet is none
    # (Set creates it, then reads it back). Read through the registry API in
    # the 64-bit view sshd reads (New-IemShellKey's): Windows PowerShell 5.1's
    # Get-Acl -LiteralPath hands a key on as its bare provider path and then
    # cannot find it (PowerShell #13107; CI run 37741639195).
    param([Parameter(Mandatory)][string]$Key, [switch]$Missing)
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $k = $base.OpenSubKey($Key.Substring('HKLM:\'.Length), [Microsoft.Win32.RegistryKeyPermissionCheck]::ReadSubTree,
            [System.Security.AccessControl.RegistryRights]::ReadPermissions)
        if ($null -eq $k) {
            if ($Missing) { return ,@() }
            return ,@("$Key does not exist")
        }
        try { $acl = $k.GetAccessControl() } finally { $k.Close() }
    } finally { $base.Close() }
    $bad = @()
    $owner = $acl.GetOwner([System.Security.Principal.SecurityIdentifier]).Value
    if ($script:KeyTrusted -notcontains $owner) { $bad += "$Key is owned by $owner" }
    $sd = [System.Security.AccessControl.RawSecurityDescriptor]::new($acl.GetSecurityDescriptorBinaryForm(), 0)
    if ($null -eq $sd.DiscretionaryAcl) { return ,($bad + @("$Key has no DACL: anyone may change it")) }
    foreach ($ace in $sd.DiscretionaryAcl) {
        if ($ace -isnot [System.Security.AccessControl.QualifiedAce]) { $bad += "$Key holds an entry of type $($ace.AceType) this check cannot read"; continue }
        if ($ace.AceQualifier -ne [System.Security.AccessControl.AceQualifier]::AccessAllowed) { continue }
        if (([int]$ace.AceFlags -band [int][System.Security.AccessControl.AceFlags]::InheritOnly) -ne 0) { continue }
        $sid = $ace.SecurityIdentifier.Value
        if ($script:KeyTrusted -contains $sid) { continue }
        $mask = ([int64]$ace.AccessMask) -band [int64]4294967295
        if (($mask -band $script:KeyChange) -ne 0) { $bad += ('{0} may be changed by {1} (0x{2:x8})' -f $Key, $sid, $mask) }
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
    # version, for this key, with the three values in a form Set-IemRegRaw
    # writes back.
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
        if (-not (Test-IemSavedRaw -Raw (Get-IemProp $values $n))) { throw "$($Paths.prior): $n is no value Set-IemRegRaw writes back: refused" }
    }
    return $doc
}

function Test-IemSavedRaw {
    # A saved value Set-IemRegRaw writes back whole: kind 'absent', or a kind
    # Get-IemRegRaw saves with its data in that kind's form (checked before any
    # value is written, so a restore never stops half-way on bad data).
    param([AllowNull()]$Raw)
    if ($null -eq $Raw) { return $false }
    $kind = [string](Get-IemProp $Raw 'kind')
    if ($kind -ceq 'absent') { return $true }
    if ($script:Kinds -cnotcontains $kind -or $null -eq $Raw.PSObject.Properties['data']) { return $false }
    $data = $Raw.data
    switch ($kind) {
        'MultiString' { return (@($data | Where-Object { $_ -isnot [string] }).Count -eq 0) }
        'DWord' {
            $n = 0
            return ($data -is [string] -and [int]::TryParse($data, [Globalization.NumberStyles]::AllowLeadingSign, [Globalization.CultureInfo]::InvariantCulture, [ref]$n))
        }
        'QWord' {
            $q = [int64]0
            return ($data -is [string] -and [int64]::TryParse($data, [Globalization.NumberStyles]::AllowLeadingSign, [Globalization.CultureInfo]::InvariantCulture, [ref]$q))
        }
        'Binary' { return ($data -is [string] -and $data -cmatch '\A([0-9a-f]{2})*\z') }
    }
    return ($data -is [string])
}

function Write-IemSavedShell {
    # The values as they are before any change of ours: written fresh beside
    # the target and moved into place (a cut write leaves only the temp file,
    # which the next write replaces), then read back (admin-only, the same values).
    param([Parameter(Mandatory)]$Paths, [Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)]$Values, [Parameter(Mandatory)][string]$UserSid)
    $doc = [pscustomobject][ordered]@{ version = $script:PriorVersion; key = $Key; saved_at = [DateTime]::UtcNow.ToString('o'); values = $Values }
    $tmp = $Paths.prior + '.tmp'
    Write-IemElevatedFile -Path $tmp -Bytes $script:Utf8NoBom.GetBytes((ConvertTo-Json -InputObject $doc -Depth 6))
    [IO.File]::Move($tmp, $Paths.prior)
    $back = Read-IemSavedShell -Paths $Paths -Key $Key -UserSid $UserSid
    if ($null -eq $back -or -not (Test-IemSameShellValues (Get-IemProp $back 'values') $Values)) { throw "$($Paths.prior) does not read back" }
    return $back
}

function Write-IemShellFile {
    # One file of the undo folder holding exactly -Bytes admin-only, read
    # back. A file that already does is kept; one whose rights read back but
    # whose bytes differ (a new build) is swapped whole (File.Replace keeps its
    # rules), so an armed task meets the old file or the new one, never part of
    # one; anything else is written fresh (Write-IemElevatedFile).
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][byte[]]$Bytes, [Parameter(Mandatory)][string]$UserSid)
    $want = Get-IemBytesSha256 -Bytes $Bytes
    $rightsOk = (Test-Path -LiteralPath $Path -PathType Leaf) -and -not (Test-IemReparsePoint -Path $Path) -and
                (Test-IemElevatedItem -Path $Path -UserSid $UserSid).Count -eq 0
    if ($rightsOk -and (Get-IemBytesSha256 -Bytes ([IO.File]::ReadAllBytes($Path))) -ceq $want) { return }
    if ($rightsOk) {
        $tmp = $Path + '.tmp'
        Write-IemElevatedFile -Path $tmp -Bytes $Bytes
        # No backup file: NullString, since PowerShell would pass $null to a .NET string as '' (IemTuningStore's journal swap).
        [IO.File]::Replace($tmp, $Path, [System.Management.Automation.Language.NullString]::Value)
    } else {
        Write-IemElevatedFile -Path $Path -Bytes $Bytes
    }
    $bad = Test-IemElevatedItem -Path $Path -UserSid $UserSid
    if ($bad.Count -gt 0) { throw ('undo file read-back: ' + ($bad -join '; ')) }
    if ((Get-IemBytesSha256 -Bytes ([IO.File]::ReadAllBytes($Path))) -cne $want) { throw "$Path does not read back" }
}

function Install-IemShellUndoFiles {
    # The undo task's folder: the entry script and copies of the three modules,
    # each read once and refused, before anything is written, unless its
    # sha256 is the one iempc checked (-ModuleSha256: the stage is shared with
    # other sessions, which stage their own builds), admin-only and read back.
    param([Parameter(Mandatory)]$Paths, [Parameter(Mandatory)][string]$UserSid, [Parameter(Mandatory)][hashtable]$ModuleSha256)
    $files = [ordered]@{}
    foreach ($m in $script:ModuleFiles) {
        $leaf = Split-Path -Leaf $m
        $want = [string]$ModuleSha256[$leaf]
        if ($want -cnotmatch '^[0-9a-f]{64}$') { throw "no sha256 for ${leaf}: refused, nothing changed" }
        $bytes = [IO.File]::ReadAllBytes($m)
        $got = Get-IemBytesSha256 -Bytes $bytes
        if ($got -cne $want) { throw "the stage's $leaf (sha256 $got) is not the build iempc checked ($want): refused, nothing changed" }
        $files[(Join-Path $Paths.dir $leaf)] = $bytes
    }
    $files[$Paths.entry] = $script:Utf8NoBom.GetBytes($script:UndoEntry)
    Install-IemElevatedFolder -Path $Paths.dir -UserSid $UserSid
    foreach ($p in @($files.Keys)) { Write-IemShellFile -Path $p -Bytes $files[$p] -UserSid $UserSid }
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

function Test-IemUndoTaskBusy {
    # The task is queued or running, or an instance of it runs.
    param($Task)
    if ($null -eq $Task) { return $false }
    if ($script:TaskBusy -contains [int]$Task.State) { return $true }
    return ([int]$Task.GetInstances(0).Count -gt 0)
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
    # script once at now + -Minutes (in UTC: a clock change moves nothing), and
    # after a missed start as soon as it can (StartWhenAvailable: a reboot in
    # between); only Administrators and SYSTEM may read, run or change it;
    # never ended hard. A folder this makes is Administrators' and SYSTEM's
    # only; an existing one (\iemmixer holds our other tasks) is kept as it is,
    # the task's own descriptor is what is read back. Returns its start time.
    param([Parameter(Mandatory)]$Paths, [Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][string]$TaskFolder,
          [Parameter(Mandatory)][string]$TaskName, [Parameter(Mandatory)][string]$UserSid, [Parameter(Mandatory)][int]$Minutes)
    $sch = Connect-IemScheduler
    $f = Get-IemTaskFolder -Scheduler $sch -Path $TaskFolder
    if ($null -eq $f) { $f = $sch.GetFolder('\').CreateFolder($TaskFolder.Trim('\'), $script:TaskSddl) }
    $ps = Join-Path ([Environment]::GetFolderPath('System')) 'WindowsPowerShell\v1.0\powershell.exe'
    $taskArgs = '-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File ' + (Format-IemArg $Paths.entry) +
        ' -Key ' + (Format-IemArg $Key) + ' -TaskFolder ' + (Format-IemArg $TaskFolder) + ' -TaskName ' + (Format-IemArg $TaskName) +
        ' -UserSid ' + (Format-IemArg $UserSid)
    $at = [DateTime]::UtcNow.AddMinutes($Minutes).ToString("yyyy-MM-dd'T'HH:mm:ss'Z'", [Globalization.CultureInfo]::InvariantCulture)
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
    $s.RunOnlyIfNetworkAvailable = $false
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
    # Everything that decides whether it can run when due.
    $bs = $back.Settings
    $runs = [bool]$bs.Enabled -and -not [bool]$bs.AllowHardTerminate -and [bool]$bs.StartWhenAvailable -and [bool]$bs.AllowDemandStart -and
            -not [bool]$bs.DisallowStartIfOnBatteries -and -not [bool]$bs.StopIfGoingOnBatteries -and -not [bool]$bs.RunOnlyIfIdle -and
            -not [bool]$bs.RunOnlyIfNetworkAvailable -and [int]$bs.MultipleInstances -eq 2 -and [string]$bs.ExecutionTimeLimit -ceq 'PT0S'
    if (-not $runs) { $bad += 'settings' }
    $sddl = [string]$task.GetSecurityDescriptor(4)
    if (-not (Test-IemUndoTaskSddl -Sddl $sddl)) { $bad += "security descriptor $sddl" }
    if ($bad.Count -gt 0) { throw ("undo task read-back ($TaskFolder\$TaskName): " + ($bad -join '; ')) }
    return $at
}

# ---- the three functions iempc and the undo task run (one at a time: Invoke-IemShellLocked) ----

function Set-IemSshShell {
    # Idempotent. Values already ours with nothing saved: `unchanged` (an undo
    # task without saved values could restore nothing and is removed).
    # Otherwise the values as they are now are saved (`set`), or a pending
    # save is kept, since it holds the values from before our first change
    # (`rearmed`); the undo files go in (the modules only as -ModuleSha256
    # says), the undo task is armed (or moved to now + -UndoMinutes); only then
    # is a missing key created admin-only, its security read, and the three
    # values written. Read back: each a REG_SZ with our data, DefaultShell an
    # existing file equal to System32\cmd.exe, and the key's security
    # (Test-IemShellKeyAcl). Refused before anything changes: an elevated root
    # that is not admin-only, a key someone else may change, a value
    # Get-IemRegRaw cannot save exactly, saved values that do not read back
    # admin-only, an undo task that is queued or runs, a module that is not the
    # checked build. A failure after the undo task was armed names it: it
    # restores the prior values at its time.
    param([Parameter(Mandatory)][hashtable]$ModuleSha256, [string]$Key = $script:DefaultKey,
          [string]$TaskFolder = $script:DefaultTaskFolder, [string]$TaskName = $script:DefaultTaskName,
          [string]$ElevatedRoot = '', [string]$User = '', [ValidateRange(2, 60)][int]$UndoMinutes = $script:UndoMinutes)
    Assert-IemShellKey -Key $Key
    Invoke-IemShellLocked {
        $u = Resolve-IemUser -User $User
        $paths = Get-IemShellPaths -ElevatedRoot $ElevatedRoot
        $rootBad = Test-IemElevatedItem -Path $paths.root -UserSid $u.sid
        if ($rootBad.Count -gt 0) { throw ('the elevated root is refused (Register-IemTasks makes it): ' + ($rootBad -join '; ')) }
        $ours = Get-IemOurShellValues
        # The key's security first, then its values: a key someone else may change is refused whatever it holds.
        $keyBad = Test-IemShellKeyAcl -Key $Key -Missing
        if ($keyBad.Count -gt 0) { throw ('refused, nothing changed: ' + ($keyBad -join '; ')) }
        $current = Get-IemShellValues -Key $Key
        $saved = Read-IemSavedShell -Paths $paths -Key $Key -UserSid $u.sid
        $task = Get-IemUndoTask -TaskFolder $TaskFolder -TaskName $TaskName
        if (Test-IemUndoTaskBusy -Task $task) {
            throw "the undo task $TaskFolder\$TaskName is queued or runs now: nothing changed; run this again once it has ended"
        }
        if ($null -eq $saved -and (Test-IemSameShellValues $current $ours)) {
            if ($null -ne $task) { [void](Remove-IemUndoTask -TaskFolder $TaskFolder -TaskName $TaskName) }
            return [pscustomobject]@{ state = 'unchanged'; key = $Key; values = $current; prior = $null; undo = $null; undo_log = (Get-IemLastUndo -Paths $paths) }
        }
        $state = 'rearmed'
        Install-IemShellUndoFiles -Paths $paths -UserSid $u.sid -ModuleSha256 $ModuleSha256
        if ($null -eq $saved) {
            $state = 'set'
            $saved = Write-IemSavedShell -Paths $paths -Key $Key -Values $current -UserSid $u.sid
        }
        $at = Register-IemUndoTask -Paths $paths -Key $Key -TaskFolder $TaskFolder -TaskName $TaskName -UserSid $u.sid -Minutes $UndoMinutes
        try {
            if (-not (Test-Path -LiteralPath $Key)) { New-IemShellKey -Key $Key }
            $keyBad = Test-IemShellKeyAcl -Key $Key
            if ($keyBad.Count -gt 0) { throw ('nothing written: ' + ($keyBad -join '; ')) }
            foreach ($n in $script:Names) { Set-IemRegRaw -Path $Key -Name $n -Raw (Get-IemProp $ours $n) }
            $read = Get-IemShellValues -Key $Key
            if (-not (Test-IemSameShellValues $read $ours)) { throw ('the values do not read back: ' + (Format-IemShellValues $read)) }
            $keyBad = Test-IemShellKeyAcl -Key $Key
            if ($keyBad.Count -gt 0) { throw ('the key reads back: ' + ($keyBad -join '; ')) }
        } catch {
            throw ("$_" + "; the undo task $TaskFolder\$TaskName stays armed: it restores the prior values at $at (UTC)")
        }
        return [pscustomobject]@{ state = $state; key = $Key; values = $read; prior = (Get-IemProp $saved 'values')
                                  undo = [pscustomobject]@{ task = ($TaskFolder.TrimEnd('\') + '\' + $TaskName); at = $at }
                                  undo_log = (Get-IemLastUndo -Paths $paths) }
    }
}

function Confirm-IemSshShell {
    # After a fresh session ran with /d: only when the three values are ours,
    # the key, the elevated root and the undo folder (with what it holds) read
    # back admin-only and the undo task is neither queued nor running. The
    # saved values go first (an undo that starts later finds nothing to
    # restore and removes itself), then the task; then the values and undo.log
    # are read again, so an undo that ran meanwhile is never reported as
    # confirmed. `confirmed`, or `unchanged` when nothing was armed.
    param([string]$Key = $script:DefaultKey, [string]$TaskFolder = $script:DefaultTaskFolder, [string]$TaskName = $script:DefaultTaskName,
          [string]$ElevatedRoot = '', [string]$User = '')
    Assert-IemShellKey -Key $Key
    Invoke-IemShellLocked {
        $u = Resolve-IemUser -User $User
        $paths = Get-IemShellPaths -ElevatedRoot $ElevatedRoot
        $ours = Get-IemOurShellValues
        $current = Get-IemShellValues -Key $Key
        if (-not (Test-IemSameShellValues $current $ours)) {
            throw ('the values are not ours (' + (Format-IemShellValues $current) + '): nothing confirmed; an armed undo task stays')
        }
        $bad = Test-IemShellKeyAcl -Key $Key
        foreach ($p in @($paths.root, $paths.dir, $paths.prior, $paths.log)) {
            if ($p -ne $paths.root -and -not (Test-Path -LiteralPath $p) -and -not (Test-IemReparsePoint -Path $p)) { continue }
            $bad += Test-IemElevatedItem -Path $p -UserSid $u.sid
        }
        if ($bad.Count -gt 0) { throw ('nothing confirmed, an armed undo task stays: ' + ($bad -join '; ')) }
        if (Test-IemUndoTaskBusy -Task (Get-IemUndoTask -TaskFolder $TaskFolder -TaskName $TaskName)) {
            throw "the undo task $TaskFolder\$TaskName is queued or runs now: nothing confirmed; run iempc ssh-shell again once it has ended"
        }
        $logBefore = Get-IemLastUndo -Paths $paths
        $removed = @()
        if (Test-Path -LiteralPath $paths.prior) {
            [IO.File]::Delete($paths.prior)
            $removed += 'saved values'
        }
        try { if (Remove-IemUndoTask -TaskFolder $TaskFolder -TaskName $TaskName) { $removed += 'undo task' } } catch {
            throw "the saved values are removed, so the undo task restores nothing; it was not removed: $($_.Exception.Message)"
        }
        $after = Get-IemShellValues -Key $Key
        if (-not (Test-IemSameShellValues $after $ours)) {
            throw ('the undo restored the prior values meanwhile (' + (Format-IemShellValues $after) + '): nothing confirmed; run iempc ssh-shell again')
        }
        $logAfter = Get-IemLastUndo -Paths $paths
        if ($logAfter -cne $logBefore) { throw "the undo task ran meanwhile ($logAfter): nothing confirmed; run iempc ssh-shell again" }
        $state = 'unchanged'
        if ($removed.Count -gt 0) { $state = 'confirmed' }
        return [pscustomobject]@{ state = $state; removed = $removed; key = $Key }
    }
}

function Undo-IemSshShell {
    # What the undo task runs (also by hand): the saved values written back
    # (Set-IemRegRaw; absent deletes the value) and read back, then the saved
    # values and the task removed: `restored`. Only from saved values that read
    # back admin-only and whole (-UserSid: the user in the elevated folders'
    # rules, which the task passes, since it runs as SYSTEM). Saved values it
    # cannot trust, or a write-back that fails, end in sshd's own default
    # instead (the three values deleted: ssh answers as before any change),
    # the saved file kept for inspection and the task removed: `default`, with
    # `why`. Nothing saved: `nothing-saved` (a leftover task is removed).
    param([string]$Key = $script:DefaultKey, [string]$TaskFolder = $script:DefaultTaskFolder, [string]$TaskName = $script:DefaultTaskName,
          [string]$ElevatedRoot = '', [string]$User = '', [string]$UserSid = '')
    Assert-IemShellKey -Key $Key
    Invoke-IemShellLocked -GoOn {
        $sid = $UserSid
        if (-not $sid) { $sid = (Resolve-IemUser -User $User).sid }
        $paths = Get-IemShellPaths -ElevatedRoot $ElevatedRoot
        $why = $null
        $saved = $null
        try { $saved = Read-IemSavedShell -Paths $paths -Key $Key -UserSid $sid } catch { $why = "$_" }
        if ($null -eq $why -and $null -eq $saved) {
            [void](Remove-IemUndoTask -TaskFolder $TaskFolder -TaskName $TaskName)
            return [pscustomobject]@{ state = 'nothing-saved'; key = $Key; values = (Get-IemShellValues -Key $Key) }
        }
        $restored = $false
        if ($null -eq $why) {
            $prior = Get-IemProp $saved 'values'
            try {
                foreach ($n in $script:Names) { Set-IemRegRaw -Path $Key -Name $n -Raw (Get-IemProp $prior $n) }
                $read = Get-IemShellValues -Key $Key
                if (-not (Test-IemSameShellValues $read $prior)) { throw ('they read back as ' + (Format-IemShellValues $read)) }
                $restored = $true
            } catch { $why = 'the saved values could not be written back: ' + "$_" }
        }
        if ($restored) {
            [IO.File]::Delete($paths.prior)
            try { [void](Remove-IemUndoTask -TaskFolder $TaskFolder -TaskName $TaskName) } catch {
                throw "the prior values are restored; the undo task $TaskFolder\$TaskName was not removed: $($_.Exception.Message)"
            }
            return [pscustomobject]@{ state = 'restored'; key = $Key; values = $read }
        }
        try {
            foreach ($n in $script:Names) { Remove-IemRegValue -Path $Key -Name $n }
            $read = Get-IemShellValues -Key $Key
            # Get-IemRegRaw's values are hashtables: their keys are no PSObject properties (Get-IemProp).
            $left = @($script:Names | Where-Object { [string](Get-IemProp $read $_).kind -cne 'absent' })
            if ($left.Count -gt 0) { throw ('they read back as ' + (Format-IemShellValues $read)) }
        } catch { throw "$why; sshd's default could not be restored either: $_" }
        try { [void](Remove-IemUndoTask -TaskFolder $TaskFolder -TaskName $TaskName) } catch {
            throw "$why; sshd's default is restored; the undo task $TaskFolder\$TaskName was not removed: $($_.Exception.Message)"
        }
        return [pscustomobject]@{ state = 'default'; why = $why; key = $Key; values = $read }
    }
}

Export-ModuleMember -Function Set-IemSshShell, Confirm-IemSshShell, Undo-IemSshShell, Test-IemUndoTaskSddl
