#Requires -Version 5.1
# S1c tuning's store (#34, split from IemTuning.psm1): the journal, the registry
# primitives and the boot identity, the layer every apply, undo, enter and exit
# of IemTuning writes through. IemTuning.psm1 imports it from its own folder
# (-Global, as IemMeasure imports IemTuning), so every admin-only path that
# stages or installs IemTuning puts this file next to it first. It calls nothing
# of IemTuning and compiles nothing (no Add-Type). Nothing here ends a process.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
# Journal schema 3 (#32 review): raw registry values, a version per tier, boot
# identities as tokens (review R1), the iemmixer plan not journaled. Schemas 1 and
# 2 are converted on read (Update-IemJournalV1, Update-IemJournalBoots) where that
# is exact, otherwise refused.
$script:Schema = 3
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding $false

function Get-IemRegPath {
    # Tests map HKLM:\... and HKCU:\... under a test key (profile registry_root).
    param([Parameter(Mandatory)]$Profile, [Parameter(Mandatory)][string]$Path)
    if (-not $Profile.registry_root) { return $Path }
    return Join-Path $Profile.registry_root ($Path -replace '^(HKLM|HKCU):\\', '$1\')
}

function Get-IemBootTime {
    (Get-CimInstance -ClassName Win32_OperatingSystem).LastBootUpTime.ToUniversalTime().ToString('o')
}

# The boot key (review R1): VOLATILE, so Windows discards it, token included, at
# every reboot. Under registry_root in the self-test.
$script:BootKeyPath = 'HKLM:\SOFTWARE\iemmixer\boot'
$script:ChildMustBeVolatile = 1021   # ERROR_CHILD_MUST_BE_VOLATILE

function Open-IemBootKey {
    # The boot key, opened for writing; created volatile when missing (its parents
    # stable, so only the leaf is volatile). A key that is not volatile (made by
    # hand, restored from an export) would outlive a reboot: refused. Windows
    # refuses a stable subkey under a volatile key, which is the probe.
    param([Parameter(Mandatory)]$Profile)
    $path = Get-IemRegPath $Profile $script:BootKeyPath
    if (-not ($path -match '^(HKLM|HKCU):\\(.+)\\([^\\]+)$')) { throw "boot key ${path}: not an HKLM: or HKCU: path" }
    $leaf = $Matches[3]
    $hive = [Microsoft.Win32.Registry]::LocalMachine
    if ($Matches[1] -eq 'HKCU') { $hive = [Microsoft.Win32.Registry]::CurrentUser }
    $parent = $hive.CreateSubKey($Matches[2])
    try {
        $key = $parent.CreateSubKey($leaf, [Microsoft.Win32.RegistryKeyPermissionCheck]::ReadWriteSubTree, [Microsoft.Win32.RegistryOptions]::Volatile)
    } finally { $parent.Close() }
    $probe = $null
    try { $probe = $key.CreateSubKey('stable-probe') } catch {
        $why = $_.Exception.GetBaseException()
        if (-not ($why -is [IO.IOException] -and ($why.HResult -band 0xFFFF) -eq $script:ChildMustBeVolatile)) {
            $key.Close()
            throw "boot key ${path}: cannot tell whether it is volatile ($($why.Message))"
        }
    }
    if ($null -ne $probe) {
        $probe.Close(); $key.DeleteSubKey('stable-probe'); $key.Close()
        throw "boot key ${path} is not volatile, so it would outlive a reboot: delete it, iemmixer then creates it volatile"
    }
    return $key
}

# The boot token's lock (#32 MINOR-3): Global\, so the first callers of a boot in
# any session (the guard's state step, an ssh apply) take the same one.
$script:BootLockName = 'Global\iemmixer-boot-token'
$script:BootLockWaitMs = 30000

function Get-IemBootIdentity {
    # This boot (review R1): a random GUID token in the volatile boot key. The key
    # holds the same token exactly while the boot that wrote it lasts, so no clock,
    # counter or service (SysMain) takes part. The time (LastBootUpTime) is
    # information only. Read and created under the boot lock (#32 MINOR-3): two
    # first callers after a boot never write two tokens (the second would journal
    # one the key no longer holds, so its Tier 3 writes would never read as
    # pending in that boot).
    param([Parameter(Mandatory)]$Profile)
    # No DACL of its own: when full access to an existing mutex is refused, the .NET
    # Framework constructor opens it with MUTEX_MODIFY_STATE | SYNCHRONIZE, which the
    # creator's default DACL grants every elevated caller and SYSTEM (review of #32).
    $lock = New-Object -TypeName System.Threading.Mutex -ArgumentList $false, $script:BootLockName
    $held = $false
    try {
        try { $held = $lock.WaitOne($script:BootLockWaitMs) }
        catch {
            # Its last holder ended without releasing it: the lock is ours now, and
            # the key's token is read again below.
            if ($_.Exception.GetBaseException() -isnot [System.Threading.AbandonedMutexException]) { throw }
            $held = $true
        }
        if (-not $held) { throw "boot token: the boot lock was not free within $($script:BootLockWaitMs / 1000) s" }
        $key = Open-IemBootKey -Profile $Profile
        try {
            $t = [string]$key.GetValue('token', '')
            $g = [guid]::Empty
            if (-not [guid]::TryParse($t, [ref]$g)) {
                $key.SetValue('token', [guid]::NewGuid().ToString(), [Microsoft.Win32.RegistryValueKind]::String)
                $t = [string]$key.GetValue('token', '')   # read back: the stored token counts
            }
        } finally { $key.Close() }
    } finally {
        if ($held) { $lock.ReleaseMutex() }
        $lock.Dispose()
    }
    return @{ token = $t; time = Get-IemBootTime }
}

function Get-IemBootIdentityOrUnknown {
    # This boot, or, when the boot key cannot be read (#32 MINOR-4), an identity
    # without a token: never this boot, so nothing reads as pending on it (the
    # direction Update-IemJournalBoots also takes), and 'problem' says why. Only
    # for what a boot-key problem must never block: undo (the revert is recorded
    # with an unknown boot) and the state (a field). A write still needs its boot.
    param([Parameter(Mandatory)]$Profile)
    try { return Get-IemBootIdentity -Profile $Profile }
    catch {
        $problem = "$_"
        $time = $null
        try { $time = Get-IemBootTime } catch { $problem += "; the boot time cannot be read either ($_)" }
        return @{ token = $null; time = $time; problem = $problem }
    }
}

function Get-IemBootToken {
    # The token of a boot identity; '' for $null or a schema 1/2 identity (a time
    # string, or { time, id }), which names no boot (review R1).
    param([AllowNull()]$Identity)
    if ($null -eq $Identity -or $Identity -is [string]) { return '' }
    if ($Identity -is [Collections.IDictionary]) { return [string]$Identity['token'] }
    if ($Identity.PSObject.Properties['token']) { return [string]$Identity.token }
    return ''
}

function Test-IemSameBoot {
    # Two boot identities name one boot exactly when both carry a token and the
    # tokens are equal (review R1); their times take no part.
    param([AllowNull()]$A, [AllowNull()]$B)
    $ta = Get-IemBootToken -Identity $A
    return ($ta -ne '' -and $ta -eq (Get-IemBootToken -Identity $B))
}

function Open-IemRegKey {
    # The key at a registry provider path opened for writing (Get-Item gives a
    # read-only handle), or $null when it does not exist. The caller closes it.
    param([Parameter(Mandatory)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    $ro = Get-Item -LiteralPath $Path
    $parts = $ro.Name -split '\\', 2
    $ro.Close()
    $hive = switch ($parts[0]) {
        'HKEY_LOCAL_MACHINE' { [Microsoft.Win32.Registry]::LocalMachine }
        'HKEY_CURRENT_USER' { [Microsoft.Win32.Registry]::CurrentUser }
        default { throw "registry hive '$($parts[0])' refused" }
    }
    $k = $hive.OpenSubKey($parts[1], $true)
    if ($null -eq $k) { throw "registry key ${Path}: not opened for writing" }
    return $k
}

function Remove-IemRegValue {
    # Deletes exactly the value Name. The name is literal: NDIS keywords start
    # with '*', which Remove-ItemProperty -Name matches as a wildcard (A2).
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$Name)
    $k = Open-IemRegKey -Path $Path
    if ($null -eq $k) { return }
    try { $k.DeleteValue($Name, $false) } finally { $k.Close() }
}

function Get-IemRegRaw {
    # One registry value exactly, as the journal keeps it for undo (A1): its
    # kind, and its data as text (numbers in decimal as Windows returns them,
    # binary as hex, a multi-string as a list), or kind 'absent'.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$Name)
    if (-not (Test-Path -LiteralPath $Path)) { return @{ kind = 'absent' } }
    $k = Get-Item -LiteralPath $Path
    $v = $k.GetValue($Name, $null, 'DoNotExpandEnvironmentNames')
    if ($null -eq $v) { return @{ kind = 'absent' } }
    $kind = "$($k.GetValueKind($Name))"
    if (@('DWord', 'QWord', 'String', 'ExpandString') -contains $kind) { return @{ kind = $kind; data = [string]$v } }
    if ($kind -eq 'MultiString') { return @{ kind = $kind; data = [string[]]@($v) } }
    if ($kind -eq 'Binary') { return @{ kind = $kind; data = (@($v | ForEach-Object { $_.ToString('x2') }) -join '') } }
    throw "registry value $Name under ${Path}: kind $kind refused"
}

function Set-IemRegRaw {
    # Writes one registry value exactly as Get-IemRegRaw reads it (kind and
    # data), or deletes exactly this value for kind 'absent'.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)]$Raw)
    $kind = [string]$Raw.kind
    if ($kind -eq 'absent') { Remove-IemRegValue -Path $Path -Name $Name; return }
    if (-not (Test-Path -LiteralPath $Path)) { New-Item -Path $Path -Force | Out-Null }
    $k = Open-IemRegKey -Path $Path
    try {
        if ($kind -eq 'DWord') { $k.SetValue($Name, [int]$Raw.data, [Microsoft.Win32.RegistryValueKind]::DWord) }
        elseif ($kind -eq 'QWord') { $k.SetValue($Name, [long]$Raw.data, [Microsoft.Win32.RegistryValueKind]::QWord) }
        elseif ($kind -eq 'String' -or $kind -eq 'ExpandString') { $k.SetValue($Name, [string]$Raw.data, [Microsoft.Win32.RegistryValueKind]$kind) }
        elseif ($kind -eq 'MultiString') { $k.SetValue($Name, [string[]]@($Raw.data), [Microsoft.Win32.RegistryValueKind]::MultiString) }
        elseif ($kind -eq 'Binary') {
            $hex = [string]$Raw.data
            if ($hex.Length % 2 -ne 0) { throw "binary data '$hex' has an odd number of hex digits" }
            $bytes = [byte[]]::new($hex.Length / 2)
            for ($i = 0; $i -lt $bytes.Length; $i++) { $bytes[$i] = [Convert]::ToByte($hex.Substring(2 * $i, 2), 16) }
            $k.SetValue($Name, $bytes, [Microsoft.Win32.RegistryValueKind]::Binary)
        } else { throw "registry kind '$kind' refused" }
    } finally { $k.Close() }
}

function Test-IemRegRawSame {
    param([Parameter(Mandatory)]$A, [Parameter(Mandatory)]$B)
    if ([string]$A.kind -ne [string]$B.kind) { return $false }
    if ([string]$A.kind -eq 'absent') { return $true }
    if ([string]$A.kind -eq 'MultiString') {
        $x = @($A.data); $y = @($B.data)
        return ($x.Count -eq $y.Count) -and (($x -join [char]0) -ceq ($y -join [char]0))
    }
    return [string]$A.data -ceq [string]$B.data
}

function Read-IemJournalFile {
    # The journal object in Path, or $null when the file is missing, empty or
    # not complete JSON with a schema.
    param([Parameter(Mandatory)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $null }
    $text = [IO.File]::ReadAllText($Path, $script:Utf8NoBom)
    if ([string]::IsNullOrWhiteSpace($text)) { return $null }
    try { $o = $text | ConvertFrom-Json } catch { return $null }
    if ($null -eq $o -or -not $o.PSObject.Properties['schema']) { return $null }
    return $o
}

function Read-IemJournal {
    # -ModeOnly: the reader needs only the mode section (exit, the fingerprint): a
    # problem in the global section does not stop it, it is in 'problems' (review 3.2).
    param([Parameter(Mandatory)][string]$Path, [switch]$ModeOnly)
    # applied: the profile version of each tier's last complete, clean apply (m2).
    # problems: what a schema-1 conversion could not convert (never written).
    $j = @{ schema = $script:Schema; applied = @{ tier2 = 0; tier3 = 0 }; entered = $false; global = @{}; mode = @{}; reverted = @{}
            order = @{ global = @(); mode = @() }; problems = @() }
    $o = Read-IemJournalFile -Path $Path
    if ($null -eq $o) {
        # A write that stopped between its flushed temp file and the swap leaves
        # the journal missing or empty next to a complete .tmp (A14). A journal
        # that exists but cannot be read, with no complete .tmp, is refused:
        # reading it as empty would lose its before-values silently.
        $o = Read-IemJournalFile -Path "$Path.tmp"
        if ($null -eq $o) {
            if (Test-Path -LiteralPath $Path) { throw "journal ${Path}: empty or unreadable, and no complete ${Path}.tmp" }
            return $j
        }
    }
    $schema = [int]$o.schema
    if (@(1, 2, $script:Schema) -notcontains $schema) { throw "journal ${Path}: schema $schema, this module $($script:Schema): not read" }
    if ($o.PSObject.Properties['applied']) { foreach ($k in 'tier2', 'tier3') { $j.applied[$k] = [int]$o.applied.$k } }
    $j.entered = [bool]$o.entered
    foreach ($s in 'global', 'mode', 'reverted') {
        foreach ($p in $o.$s.PSObject.Properties) { $j[$s][$p.Name] = $p.Value }
    }
    foreach ($s in 'global', 'mode') { $j.order[$s] = @($o.order.$s | Where-Object { $_ }) }
    if ($schema -eq 1) {
        # A schema-1 journal that could not be converted fully stays schema 1 when
        # written, so its refusal stays until the entry is resolved by hand.
        $j.problems = Update-IemJournalV1 -Journal $j -Path $Path
        if (@($j.problems).Count -gt 0) { $j.schema = 1 }
    }
    if ($schema -lt $script:Schema) { Update-IemJournalBoots -Journal $j }
    $blocking = @(@($j.problems) | Where-Object { -not $ModeOnly -or $_.section -eq 'mode' })
    if ($blocking.Count -gt 0) { throw (@($blocking | ForEach-Object { $_.text }) -join '; ') }
    return $j
}

function Update-IemJournalV1 {
    # A schema-1 journal (before the #32 review) becomes schema 3 only where the
    # conversion is exact (m1): its one 'version' is dropped, so both tiers stay at
    # applied version 0 and every held tier reports drift until applied again (the
    # tier it named is unknown); its boot times are converted by
    # Update-IemJournalBoots; a registry entry whose before-value was
    # absent gets raw 'absent'; the plan-exists / plan-value mode entries are
    # dropped (the plan stays defined and is never reverted now, A6/M2). A registry
    # entry with a before-value but no kind cannot be restored exactly: it stays as
    # it is and is returned as a problem of its section, naming the file and the
    # entry (Read-IemJournal refuses the journal for it, review 3.2).
    param([Parameter(Mandatory)][hashtable]$Journal, [Parameter(Mandatory)][string]$Path)
    $problems = @()
    foreach ($s in 'global', 'mode') {
        foreach ($k in @($Journal[$s].Keys)) {
            $e = $Journal[$s][$k]
            if ($s -eq 'mode' -and @('plan-exists', 'plan-value') -contains [string]$e.kind) {
                $Journal[$s].Remove($k)
                $Journal.order[$s] = @($Journal.order[$s] | Where-Object { $_ -ne $k })
                continue
            }
            if ([string]$e.kind -eq 'reg' -and -not $e.PSObject.Properties['raw']) {
                if ($null -ne $e.before) {
                    $problems += [pscustomobject]@{ section = $s; text = "journal ${Path}: schema 1, entry '$k' holds a registry before-value without its kind, so it cannot be restored exactly: restore it by hand and remove the entry" }
                    continue
                }
                $e | Add-Member -NotePropertyName 'raw' -NotePropertyValue @{ kind = 'absent' }
            }
        }
    }
    return ,$problems
}

function ConvertFrom-IemOldBoot {
    # A schema 1/2 boot identity (a time string, or { time, id }) as one without a
    # token; an identity that has a token field stays as it is.
    param([AllowNull()]$Boot)
    if ($null -eq $Boot) { return $null }
    if ($Boot -is [string]) { return @{ token = $null; time = $Boot } }
    if ($Boot.PSObject.Properties['token']) { return $Boot }
    $time = $null
    if ($Boot.PSObject.Properties['time']) { $time = [string]$Boot.time }
    return @{ token = $null; time = $time }
}

function Update-IemJournalBoots {
    # Schema 1 and 2 boot identities (a boot time; { time, id } with Windows'
    # BootId counter) cannot prove a boot, so each becomes an identity without a
    # token: never this boot (review R1). An older module's write then never reads
    # as 'pending', the direction that never prescribes a revert reboot (a false
    # 'pending' does, in post_boot_verdict). The time stays as information.
    param([Parameter(Mandatory)][hashtable]$Journal)
    foreach ($s in 'global', 'mode') {
        foreach ($k in @($Journal[$s].Keys)) {
            $e = $Journal[$s][$k]
            if ($null -ne $e -and $e.PSObject.Properties['boot']) { $e.boot = ConvertFrom-IemOldBoot -Boot $e.boot }
        }
    }
    foreach ($k in @($Journal.reverted.Keys)) { $Journal.reverted[$k] = ConvertFrom-IemOldBoot -Boot $Journal.reverted[$k] }
}

function Write-IemJournal {
    # The temp file is written through to the disk, then swapped in with
    # File.Replace, which keeps journal.json under its name until the new one
    # takes it (Move-Item -Force on 5.1 deletes it first). A stop in between
    # leaves a complete .tmp that Read-IemJournal falls back to (A14).
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][hashtable]$Journal)
    $dir = Split-Path -Parent $Path
    if (-not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    $tmp = "$Path.tmp"
    $data = $Journal.Clone(); $data.Remove('problems')   # read-time findings, never stored
    $bytes = $script:Utf8NoBom.GetBytes(($data | ConvertTo-Json -Depth 8))
    $fs = New-Object -TypeName IO.FileStream -ArgumentList $tmp, ([IO.FileMode]::Create), ([IO.FileAccess]::Write), ([IO.FileShare]::None), 4096, ([IO.FileOptions]::WriteThrough)
    try { $fs.Write($bytes, 0, $bytes.Length); $fs.Flush($true) } finally { $fs.Dispose() }
    if (Test-Path -LiteralPath $Path) { [IO.File]::Replace($tmp, $Path, [System.Management.Automation.Language.NullString]::Value) }
    else { [IO.File]::Move($tmp, $Path) }
}

Export-ModuleMember -Function *-Iem*
