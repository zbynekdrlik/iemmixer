#Requires -Version 5.1
# S1c Windows tuning (docs/superpowers/specs/2026-09-27-s1c-windows-tuning-design.md section 6, section 7).
# Every change is an item: a kind with arguments and a desired value, read by
# Get-IemValue and written by Set-IemValue. Apply writes only what differs,
# journals the value before the first write and reads back; undo and exit
# write the journaled value back and read back. Values are strings; $null is
# "absent". Nothing here ends a process, forces a service or restarts Windows.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
# Journal schema 2 (#32 review): raw registry values, a version per tier, boot
# identities, the iemmixer plan not journaled. Schema 1 is converted on read where
# that is exact (Update-IemJournalV1), otherwise refused.
$script:Schema = 2
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding $false

if (-not ('IemPower' -as [type])) {
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;

public static class IemPower {
    [DllImport("powrprof.dll")] static extern uint PowerGetActiveScheme(IntPtr root, out IntPtr scheme);
    [DllImport("powrprof.dll")] static extern uint PowerSetActiveScheme(IntPtr root, ref Guid scheme);
    [DllImport("powrprof.dll")] static extern uint PowerReadACValueIndex(IntPtr root, ref Guid scheme, ref Guid sub, ref Guid setting, out uint value);
    [DllImport("powrprof.dll")] static extern uint PowerWriteACValueIndex(IntPtr root, ref Guid scheme, ref Guid sub, ref Guid setting, uint value);
    [DllImport("powrprof.dll")] static extern uint PowerReadFriendlyName(IntPtr root, ref Guid scheme, IntPtr sub, IntPtr setting, byte[] buffer, ref uint size);
    [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr p);

    public static string Active() {
        IntPtr p;
        uint rc = PowerGetActiveScheme(IntPtr.Zero, out p);
        if (rc != 0) throw new Win32Exception((int)rc);
        try { return ((Guid)Marshal.PtrToStructure(p, typeof(Guid))).ToString(); } finally { LocalFree(p); }
    }
    public static void Activate(string scheme) {
        Guid g = new Guid(scheme);
        uint rc = PowerSetActiveScheme(IntPtr.Zero, ref g);
        if (rc != 0) throw new Win32Exception((int)rc);
    }
    // -1 when the scheme or the setting does not exist.
    public static long Read(string scheme, string sub, string setting) {
        Guid a = new Guid(scheme), b = new Guid(sub), c = new Guid(setting);
        uint v;
        return PowerReadACValueIndex(IntPtr.Zero, ref a, ref b, ref c, out v) == 0 ? (long)v : -1;
    }
    public static void Write(string scheme, string sub, string setting, uint value) {
        Guid a = new Guid(scheme), b = new Guid(sub), c = new Guid(setting);
        uint rc = PowerWriteACValueIndex(IntPtr.Zero, ref a, ref b, ref c, value);
        if (rc != 0) throw new Win32Exception((int)rc);
    }
    // The scheme's friendly name (a language-neutral read); null only for
    // ERROR_FILE_NOT_FOUND (no such scheme), any other failure throws.
    public static string Name(string scheme) {
        Guid g = new Guid(scheme);
        uint size = 0;
        uint rc = PowerReadFriendlyName(IntPtr.Zero, ref g, IntPtr.Zero, IntPtr.Zero, null, ref size);
        if (rc == 2) return null;
        if (rc != 0) throw new Win32Exception((int)rc);
        byte[] buf = new byte[size];
        rc = PowerReadFriendlyName(IntPtr.Zero, ref g, IntPtr.Zero, IntPtr.Zero, buf, ref size);
        if (rc != 0) throw new Win32Exception((int)rc);
        return System.Text.Encoding.Unicode.GetString(buf, 0, (int)size).TrimEnd((char)0);
    }
}

public static class IemCpuSets {
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool GetSystemCpuSetInformation(IntPtr info, uint length, out uint returned, IntPtr process, uint flags);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool GetProcessDefaultCpuSets(IntPtr process, uint[] ids, uint count, out uint required);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool SetProcessDefaultCpuSets(IntPtr process, uint[] ids, uint count);
    [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    const uint QueryLimited = 0x1000, SetLimited = 0x2000;

    // Group 0: logical processor index -> CPU Set ID.
    public static Dictionary<int, uint> Map() {
        uint len;
        GetSystemCpuSetInformation(IntPtr.Zero, 0, out len, IntPtr.Zero, 0);
        IntPtr buf = Marshal.AllocHGlobal((int)len);
        try {
            if (!GetSystemCpuSetInformation(buf, len, out len, IntPtr.Zero, 0)) throw new Win32Exception();
            var map = new Dictionary<int, uint>();
            int off = 0;
            while (off + 16 <= (int)len) {
                int size = Marshal.ReadInt32(buf, off);
                if (size <= 0) break;
                if (Marshal.ReadInt32(buf, off + 4) == 0 && Marshal.ReadInt16(buf, off + 12) == 0) {
                    map[Marshal.ReadByte(buf, off + 14)] = (uint)Marshal.ReadInt32(buf, off + 8);
                }
                off += size;
            }
            return map;
        } finally { Marshal.FreeHGlobal(buf); }
    }
    static IntPtr Open(int pid, uint access) {
        IntPtr h = OpenProcess(access, false, pid);
        if (h == IntPtr.Zero) throw new Win32Exception();
        return h;
    }
    public static uint[] Get(int pid) {
        IntPtr h = Open(pid, QueryLimited);
        try {
            uint needed;
            if (GetProcessDefaultCpuSets(h, null, 0, out needed)) return new uint[0];
            if (Marshal.GetLastWin32Error() != 122) throw new Win32Exception();
            uint[] ids = new uint[needed];
            if (!GetProcessDefaultCpuSets(h, ids, needed, out needed)) throw new Win32Exception();
            return ids;
        } finally { CloseHandle(h); }
    }
    public static void Set(int pid, uint[] ids) {
        if (ids == null) ids = new uint[0];
        IntPtr h = Open(pid, SetLimited);
        try {
            if (!SetProcessDefaultCpuSets(h, ids.Length == 0 ? null : ids, (uint)ids.Length)) throw new Win32Exception();
        } finally { CloseHandle(h); }
    }
}
'@
}

# The iemmixer plan's settings (design note 6.2 L2): subgroup, setting, AC value.
$script:PlanSettings = @(
    @{ name = 'proc-min'; sub = '54533251-82be-4824-96c1-47b60b740d00'; setting = '893dee8e-2bef-41e0-89c6-b55d0929964c'; value = 100 },
    @{ name = 'proc-max'; sub = '54533251-82be-4824-96c1-47b60b740d00'; setting = 'bc5038f7-23e0-4960-96da-33abaf5935ec'; value = 100 },
    @{ name = 'park-min-cores'; sub = '54533251-82be-4824-96c1-47b60b740d00'; setting = '0cc5b647-c1df-4637-891a-dec35c318583'; value = 100 },
    @{ name = 'park-max-cores'; sub = '54533251-82be-4824-96c1-47b60b740d00'; setting = 'ea062031-0e34-4ff1-9b6d-eb1059334028'; value = 100 },
    @{ name = 'epp'; sub = '54533251-82be-4824-96c1-47b60b740d00'; setting = '36687f9e-e3a5-4dbf-b1dc-15eb381c6863'; value = 0 },
    @{ name = 'pcie-aspm'; sub = '501a4d13-42af-4429-9fd1-a8218c268e20'; setting = 'ee12f906-d277-404b-b6da-e5fa1a576df5'; value = 0 },
    @{ name = 'usb-suspend'; sub = '2a737441-1930-4402-8d77-b2bebba308a3'; setting = '48e6b7a6-50f5-4782-a5d4-53bb8f07e226'; value = 0 },
    @{ name = 'display-off'; sub = '7516b95f-f776-4464-8c53-06167f40cc99'; setting = '3c0bc021-c8a8-4e07-a973-6b14cbcb2b7e'; value = 0 },
    @{ name = 'disk-off'; sub = '0012ee47-9041-4b5d-9b77-535fba8b1442'; setting = '6738e2c4-e8a5-4a42-b16a-e040e769756e'; value = 0 },
    @{ name = 'sleep'; sub = '238c9fa8-0aad-41ed-83f4-97be242c8f20'; setting = '29f6c1db-86da-48c5-9fdb-f2b67b1f44da'; value = 0 },
    @{ name = 'hibernate'; sub = '238c9fa8-0aad-41ed-83f4-97be242c8f20'; setting = '9d7815a6-7ee4-497e-8888-515a05f02364'; value = 0 }
)
$script:ProcessorSub = '54533251-82be-4824-96c1-47b60b740d00'
$script:IdleDisable = '5d76a2ca-e8c0-402f-a133-2158492d58ad'
$script:IdleStateMax = '9943e905-9a30-4ec1-9b99-44dd3b76f7a2'
# iemmixer's own plan carries this name, set right after /duplicatescheme: an
# existing plan is written into only when it has it (M2, m6).
$script:PlanName = 'iemmixer'
$script:PlanDescription = 'iemmixer tuning plan (S1c, design note 6.2 L2)'
# Windows' built-in schemes (Balanced, High performance, Power saver, Ultimate
# Performance): never iemmixer's plan.
$script:BuiltinSchemes = @('381b4222-f694-41f0-9685-ff5bb260df2e', '8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c',
                           'a1841308-3541-4fab-bc81-f71556f20b4a', 'e9a42b02-d5df-448d-aa00-03f14749eb61')
$script:NetClass = 'HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4d36e972-e325-11ce-bfc1-08002be10318}'

function Read-IemProfile {
    param([Parameter(Mandatory)][string]$Path)
    $p = Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
    foreach ($k in 'version', 'journal', 'registry_root', 'layout', 'plan', 'governor', 'placement', 'services_disable', 'services_mode',
                   'updates', 'maintenance', 'defender', 'devices', 'nic', 'fingerprint') {
        if (-not $p.PSObject.Properties[$k]) { throw "profile ${Path}: missing '$k'" }
    }
    if ([int]$p.version -lt 1) { throw "profile ${Path}: version must be a positive integer" }
    return $p
}

function Assert-IemLayout {
    # The layout's roles are disjoint (one processor, one role), as the window
    # requires. Checked before a write (apply, enter), never on the exit path.
    param([Parameter(Mandatory)]$Profile)
    $seen = @{}
    foreach ($role in 'housekeeping', 'card', 'nic', 'audio') {
        if (-not $Profile.layout.PSObject.Properties[$role]) { continue }
        foreach ($lp in @(@($Profile.layout.$role) | Where-Object { $null -ne $_ })) {
            $k = [string][int]$lp
            if ($seen.ContainsKey($k)) { throw "layout: processor $k is in both $($seen[$k]) and $role (roles overlap)" }
            $seen[$k] = $role
        }
    }
}

function Get-IemRegPath {
    # Tests map HKLM:\... and HKCU:\... under a test key (profile registry_root).
    param([Parameter(Mandatory)]$Profile, [Parameter(Mandatory)][string]$Path)
    if (-not $Profile.registry_root) { return $Path }
    return Join-Path $Profile.registry_root ($Path -replace '^(HKLM|HKCU):\\', '$1\')
}

function Get-IemBootTime {
    (Get-CimInstance -ClassName Win32_OperatingSystem).LastBootUpTime.ToUniversalTime().ToString('o')
}

# Two boot-time readings this close can be the same boot (A13). A clock step (time
# sync) moves LastBootUpTime by the step, typically seconds; a reboot moves it by
# at least the whole previous session.
$script:BootToleranceSeconds = 300
$script:BootIdKey = 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management\PrefetchParameters'

function Get-IemBootIdentity {
    # This boot: its time and Windows' BootId counter (PrefetchParameters\BootId,
    # raised at every boot; $null where Windows keeps none) (m5). The counter lives
    # in the prefetcher's registry area and may stop advancing once Tier 2 disables
    # SysMain: harmless while 'pending' only reports, and Test-IemSameBoot's time
    # rule still tells two boots apart then.
    $id = $null
    if (Test-Path -LiteralPath $script:BootIdKey) {
        $v = (Get-Item -LiteralPath $script:BootIdKey).GetValue('BootId', $null)
        if ($null -ne $v) { $id = [long]$v }
    }
    return @{ time = Get-IemBootTime; id = $id }
}

function Test-IemSameBoot {
    # Two boot identities ({ time, id }) name the same boot when their BootId
    # counters, where both have one, are equal (a reboot raises it however quick,
    # m5) AND their times lie within 300 s (a clock step, A13). The time rule is
    # the fallback: it decides alone without a counter, and it still separates two
    # boots when the counter did not advance (it may freeze once SysMain is off);
    # only a reboot within 300 s with a frozen counter would read as one boot.
    param([AllowNull()]$A, [AllowNull()]$B)
    if ($null -eq $A -or $null -eq $B) { return $false }
    $ta = [string]$A.time; $tb = [string]$B.time
    if ([string]::IsNullOrEmpty($ta) -or [string]::IsNullOrEmpty($tb)) { return $false }
    if ($null -ne $A.id -and $null -ne $B.id -and [string]$A.id -ne [string]$B.id) { return $false }
    $c = [Globalization.CultureInfo]::InvariantCulture
    $s = [Globalization.DateTimeStyles]::RoundtripKind
    $d = [datetime]::Parse($ta, $c, $s).ToUniversalTime() - [datetime]::Parse($tb, $c, $s).ToUniversalTime()
    return [math]::Abs($d.TotalSeconds) -le $script:BootToleranceSeconds
}

function Invoke-IemNative {
    # Runs a native program; returns its exit code and its stdout and stderr lines
    # as text. Under 'Stop', Windows PowerShell 5.1 turns the first stderr line of
    # a 2>&1 redirect into a terminating error before the exit code is known, so
    # this scope continues on stderr and the exit code alone decides (A11).
    param([Parameter(Mandatory)][string]$FilePath, [string[]]$Arguments = @())
    if ([IO.Path]::IsPathRooted($FilePath)) {
        if (-not (Test-Path -LiteralPath $FilePath -PathType Leaf)) { throw "$FilePath not found" }
    } else { [void](Get-Command -Name $FilePath -CommandType Application -ErrorAction Stop) }
    $ErrorActionPreference = 'Continue'
    $out = @(& $FilePath @Arguments 2>&1 | ForEach-Object { "$_" })
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    return [pscustomobject]@{ code = $code; out = $out }
}

function Test-IemSame {
    param([AllowNull()]$A, [AllowNull()]$B)
    if ($null -eq $A) { return $null -eq $B }
    if ($null -eq $B) { return $false }
    return [string]$A -eq [string]$B
}

function New-IemItem {
    # -NoJournal: an item that is only ensured, never reverted (the iemmixer
    # plan's existence and settings: it stays defined, design note 6.2 L2).
    param([Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][string]$Kind, [Parameter(Mandatory)][hashtable]$Arguments,
          [AllowNull()]$Desired, [int]$Tier = 0, [string]$Group = '', [switch]$Reboot, [switch]$NoJournal)
    [pscustomobject]@{ key = $Key; kind = $Kind; args = $Arguments; tier = $Tier; group = $Group; reboot = [bool]$Reboot
                       journal = -not $NoJournal.IsPresent; desired = $(if ($null -eq $Desired) { $null } else { [string]$Desired }) }
}

function Test-IemPlan {
    param([Parameter(Mandatory)][string]$Guid)
    return [bool](@(& powercfg.exe /list) -match [regex]::Escape($Guid))
}

function Get-IemDefenderList {
    param([Parameter(Mandatory)][ValidateSet('ExclusionPath', 'ExclusionProcess')][string]$Name)
    return @((Get-MpPreference).$Name | Where-Object { $_ })
}

function Assert-IemSameProcess {
    # A journaled pid is only this process while its name and start time match.
    param([Parameter(Mandatory)]$Arguments)
    $p = Get-Process -Id ([int]$Arguments.pid) -ErrorAction SilentlyContinue
    if (-not $p -or $p.ProcessName -ne $Arguments.name -or $p.StartTime.ToUniversalTime().Ticks -ne [long]$Arguments.start) {
        throw "process $($Arguments.name) ($($Arguments.pid)) is gone or its pid was reused"
    }
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

function Get-IemValue {
    param([Parameter(Mandatory)]$Item)
    $a = $Item.args
    switch ($Item.kind) {
        'reg' {
            if (-not (Test-Path -LiteralPath $a.path)) { return $null }
            $v = (Get-Item -LiteralPath $a.path).GetValue($a.name, $null, 'DoNotExpandEnvironmentNames')
            if ($null -eq $v) { return $null }
            if ($v -is [byte[]]) { return (($v | ForEach-Object { $_.ToString('x2') }) -join '') }
            return [string]$v
        }
        'svc-start' {
            $key = "HKLM:\SYSTEM\CurrentControlSet\Services\$($a.name)"
            if (-not (Test-Path -LiteralPath $key)) { return $null }
            $k = Get-Item -LiteralPath $key
            $start = [int]$k.GetValue('Start', -1)
            if ($start -eq 2 -and [int]$k.GetValue('DelayedAutostart', 0) -eq 1) { return 'delayed-auto' }
            $names = @{ 0 = 'boot'; 1 = 'system'; 2 = 'auto'; 3 = 'demand'; 4 = 'disabled' }
            if ($names.ContainsKey($start)) { return $names[$start] }
            return "start-$start"
        }
        'svc-state' {
            $s = Get-Service -Name $a.name -ErrorAction SilentlyContinue
            if (-not $s) { return $null }
            if ($s.Status -eq 'Running') { return 'running' }
            return 'stopped'
        }
        'task' {
            $t = Get-ScheduledTask -TaskPath $a.path -TaskName $a.name -ErrorAction SilentlyContinue
            if (-not $t) { return $null }
            if ("$($t.State)" -eq 'Disabled') { return 'disabled' }
            return 'enabled'
        }
        'plan-exists' { if (Test-IemPlan -Guid $a.guid) { return 'present' }; return $null }
        'plan-value' {
            $v = [IemPower]::Read($a.guid, $a.sub, $a.setting)
            if ($v -lt 0) { return $null }
            return [string]$v
        }
        'plan-active' { return [IemPower]::Active() }
        'skip' { return [string]$a.reason }   # a lever the profile names but the device state excludes
        'defender-path' { if ((Get-IemDefenderList -Name 'ExclusionPath') -contains $a.value) { return 'present' }; return $null }
        'defender-process' { if ((Get-IemDefenderList -Name 'ExclusionProcess') -contains $a.value) { return 'present' }; return $null }
        'cpusets' {
            try { Assert-IemSameProcess -Arguments $a } catch { return $null }
            return ((@([IemCpuSets]::Get([int]$a.pid)) | Sort-Object) -join ',')
        }
        default { throw "unknown item kind '$($Item.kind)'" }
    }
}

function Set-IemValue {
    param([Parameter(Mandatory)]$Item, [AllowNull()]$Value)
    $a = $Item.args
    switch ($Item.kind) {
        'reg' {
            if ($null -eq $Value) { Remove-IemRegValue -Path $a.path -Name $a.name; return }
            if (@('DWord', 'QWord', 'String', 'Binary') -notcontains [string]$a.type) { throw "registry type '$($a.type)' refused" }
            Set-IemRegRaw -Path $a.path -Name $a.name -Raw @{ kind = [string]$a.type; data = [string]$Value }
        }
        'svc-start' {
            if (@('auto', 'delayed-auto', 'demand', 'disabled') -notcontains [string]$Value) { throw "service start type '$Value' refused for $($a.name)" }
            $r = Invoke-IemNative -FilePath 'sc.exe' -Arguments @('config', $a.name, 'start=', [string]$Value)
            if ($r.code -ne 0) { throw "sc.exe config $($a.name) start= ${Value}: $($r.out -join ' ')" }
        }
        'svc-state' {
            $s = Get-Service -Name $a.name
            if ($Value -eq 'running') { Start-Service -InputObject $s; $s.WaitForStatus('Running', [TimeSpan]::FromSeconds(60)) }
            elseif ($Value -eq 'stopped') { Stop-Service -InputObject $s; $s.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(60)) }
            else { throw "service state '$Value' refused" }
        }
        'task' {
            if ($Value -eq 'disabled') { Disable-ScheduledTask -TaskPath $a.path -TaskName $a.name | Out-Null }
            elseif ($Value -eq 'enabled') { Enable-ScheduledTask -TaskPath $a.path -TaskName $a.name | Out-Null }
            else { throw "task state '$Value' refused" }
        }
        'plan-exists' {
            # The plan is created once and stays defined (design note 6.2 L2): never deleted here.
            if ($Value -ne 'present') { throw "plan $($a.guid): only 'present' is written" }
            $r = Invoke-IemNative -FilePath 'powercfg.exe' -Arguments @('/duplicatescheme', $a.source, $a.guid)
            if ($r.code -ne 0) { throw "powercfg /duplicatescheme: $($r.out -join ' ')" }
            # Named right away: an existing plan counts as iemmixer's only by this name
            # (M2, m6). A failed rename or read-back deletes the plan this call just
            # created, so no half-made plan stays under the source's name (review 3.3).
            try {
                $r = Invoke-IemNative -FilePath 'powercfg.exe' -Arguments @('/changename', $a.guid, $script:PlanName, $script:PlanDescription)
                if ($r.code -ne 0) { throw "powercfg /changename: $($r.out -join ' ')" }
                $n = Get-IemPlanName -Guid ([string]$a.guid)
                if ($n -cne $script:PlanName) { throw "its name reads back '$n', not '$($script:PlanName)'" }
            } catch {
                $why = "$_"
                $d = Invoke-IemNative -FilePath 'powercfg.exe' -Arguments @('/delete', $a.guid)
                if ($d.code -ne 0) { throw "plan $($a.guid): $why; deleting the plan this call created failed too ($($d.out -join ' ')): delete it by hand" }
                throw "plan $($a.guid): $why; the plan this call had just created was deleted"
            }
        }
        'plan-value' {
            if ($null -eq $Value) { throw 'a plan value cannot be removed' }
            if ((Get-IemPlanName -Guid ([string]$a.guid)) -cne $script:PlanName) { throw "plan $($a.guid) is not iemmixer's own plan: value not written (M2)" }
            [IemPower]::Write($a.guid, $a.sub, $a.setting, [uint32]$Value)
        }
        'plan-active' { [IemPower]::Activate([string]$Value) }
        'skip' { throw "$($Item.key): a skipped lever is never written" }
        'defender-path' { if ($Value -eq 'present') { Add-MpPreference -ExclusionPath $a.value } else { Remove-MpPreference -ExclusionPath $a.value } }
        'defender-process' { if ($Value -eq 'present') { Add-MpPreference -ExclusionProcess $a.value } else { Remove-MpPreference -ExclusionProcess $a.value } }
        'cpusets' {
            Assert-IemSameProcess -Arguments $a
            # A typed variable initialised to @() keeps an empty array; the result
            # of an `if` unrolls to $null and would crash IemCpuSets.Set (S1c review).
            [uint32[]]$ids = @()
            if (-not [string]::IsNullOrEmpty([string]$Value)) { $ids = @(([string]$Value) -split ',' | ForEach-Object { [uint32]$_ }) }
            [IemCpuSets]::Set([int]$a.pid, $ids)
        }
        default { throw "unknown item kind '$($Item.kind)'" }
    }
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
    if ($schema -ne $script:Schema -and $schema -ne 1) { throw "journal ${Path}: schema $schema, this module $($script:Schema): not read" }
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
    $blocking = @(@($j.problems) | Where-Object { -not $ModeOnly -or $_.section -eq 'mode' })
    if ($blocking.Count -gt 0) { throw (@($blocking | ForEach-Object { $_.text }) -join '; ') }
    return $j
}

function Update-IemJournalV1 {
    # A schema-1 journal (before the #32 review) becomes schema 2 only where the
    # conversion is exact (m1): its one 'version' is dropped, so both tiers stay at
    # applied version 0 and every held tier reports drift until applied again (the
    # tier it named is unknown); a boot time string becomes an identity without a
    # counter (the A13 tolerance decides); a registry entry whose before-value was
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
            if ($e.boot -is [string]) { $e.boot = @{ time = $e.boot; id = $null } }
            if ([string]$e.kind -eq 'reg' -and -not $e.PSObject.Properties['raw']) {
                if ($null -ne $e.before) {
                    $problems += [pscustomobject]@{ section = $s; text = "journal ${Path}: schema 1, entry '$k' holds a registry before-value without its kind, so it cannot be restored exactly: restore it by hand and remove the entry" }
                    continue
                }
                $e | Add-Member -NotePropertyName 'raw' -NotePropertyValue @{ kind = 'absent' }
            }
        }
    }
    foreach ($k in @($Journal.reverted.Keys)) {
        if ($Journal.reverted[$k] -is [string]) { $Journal.reverted[$k] = @{ time = $Journal.reverted[$k]; id = $null } }
    }
    return ,$problems
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

function ConvertTo-IemItem {
    # An item rebuilt from its journal entry, desired = the journaled before-value;
    # restore = the exact registry value (kind and data) for a 'reg' item (A1).
    param([Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)]$Entry)
    $raw = $null
    if ($Entry.PSObject.Properties['raw']) { $raw = $Entry.raw }
    [pscustomobject]@{ key = $Key; kind = $Entry.kind; args = $Entry.args; tier = [int]$Entry.tier; group = [string]$Entry.group
                       reboot = [bool]$Entry.reboot; desired = $Entry.before; restore = $raw }
}

function Invoke-IemItem {
    # Write one item only when it differs; journal its value before the first
    # write (saved before the write), then read back. An optional target that
    # does not exist (a task or service missing on this edition) is 'absent'.
    param([Parameter(Mandatory)]$Item, [Parameter(Mandatory)][hashtable]$Journal, [Parameter(Mandatory)][string]$Section,
          [Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)]$Boot)
    $row = [ordered]@{ key = $Item.key; tier = $Item.tier; group = $Item.group; action = ''; before = $null; value = $null; error = $null }
    if ($Item.kind -eq 'skip') { $row.action = 'skipped'; $row.value = $Item.desired; return [pscustomobject]$row }
    try {
        $before = Get-IemValue -Item $Item
        $row.before = $before
        if ($null -eq $before -and @('task', 'svc-start', 'svc-state', 'cpusets') -contains $Item.kind) { $row.action = 'absent'; return [pscustomobject]$row }
        if (Test-IemSame $before $Item.desired) { $row.action = 'kept'; $row.value = $before; return [pscustomobject]$row }
        $e = $Journal[$Section][$Item.key]
        if (-not $Item.journal) {
            # Ensured only: nothing to revert.
        } elseif ($null -eq $e) {
            $e = @{ kind = $Item.kind; args = $Item.args; before = $before; tier = $Item.tier; group = $Item.group
                    reboot = $Item.reboot; at = (Get-Date).ToUniversalTime().ToString('o'); boot = $Boot }
            # The exact value for undo: its registry kind and data, not the text (A1).
            if ($Item.kind -eq 'reg') { $e.raw = Get-IemRegRaw -Path $Item.args.path -Name $Item.args.name }
            $Journal[$Section][$Item.key] = $e
            $Journal.order[$Section] = @($Journal.order[$Section]) + $Item.key
            Write-IemJournal -Path $Path -Journal $Journal
        } else {
            # The entry names its target: when the key now points elsewhere (a NIC
            # driver key recreated, a card in another slot) the new target's value
            # would go unjournaled, so the write is refused (review 3.8).
            if ($Item.kind -eq 'reg' -and ([string]$e.args.path -ne [string]$Item.args.path -or [string]$e.args.name -ne [string]$Item.args.name)) {
                throw "the journal holds '$($Item.key)' for $($e.args.path) $($e.args.name), the profile now names $($Item.args.path) $($Item.args.name): undo this tier first"
            }
            # The before-value stays the first one; the boot is the latest write's,
            # so a value re-written after a reboot is pending again (A3).
            if ([string]$e.boot.time -ne [string]$Boot.time -or [string]$e.boot.id -ne [string]$Boot.id) {
                $e.boot = $Boot
                Write-IemJournal -Path $Path -Journal $Journal
            }
        }
        Set-IemValue -Item $Item -Value $Item.desired
        $after = Get-IemValue -Item $Item
        if (-not (Test-IemSame $after $Item.desired)) { throw "read back '$after' after writing '$($Item.desired)'" }
        $row.action = 'written'; $row.value = $after
    } catch { $row.action = 'failed'; $row.error = "$_" }
    return [pscustomobject]$row
}

function Restore-IemItem {
    # Write the journaled before-value back and read it back. A placed process
    # that ended (or whose pid was reused) is 'gone': nothing to restore.
    param([Parameter(Mandatory)]$Item)
    if ($Item.kind -eq 'reg') { return Restore-IemRegItem -Item $Item }
    $now = Get-IemValue -Item $Item
    if ($Item.kind -eq 'cpusets' -and $null -eq $now) { return [pscustomobject]@{ key = $Item.key; action = 'gone'; value = $null; error = $null } }
    if (Test-IemSame $now $Item.desired) { return [pscustomobject]@{ key = $Item.key; action = 'kept'; value = $now; error = $null } }
    Set-IemValue -Item $Item -Value $Item.desired
    $after = Get-IemValue -Item $Item
    if (-not (Test-IemSame $after $Item.desired)) { throw "$($Item.key): read back '$after' after restoring '$($Item.desired)'" }
    return [pscustomobject]@{ key = $Item.key; action = 'restored'; value = $after; error = $null }
}

function Restore-IemRegItem {
    # A registry value goes back to its journaled kind and data (A1); the item's
    # own type would turn a REG_BINARY mask into a wrong QWORD.
    param([Parameter(Mandatory)]$Item)
    $a = $Item.args
    if ($null -eq $Item.restore) { throw "$($Item.key): the journal entry has no exact registry value (older module): restore it by hand" }
    if (Test-IemRegRawSame -A (Get-IemRegRaw -Path $a.path -Name $a.name) -B $Item.restore) {
        return [pscustomobject]@{ key = $Item.key; action = 'kept'; value = $Item.desired; error = $null }
    }
    Set-IemRegRaw -Path $a.path -Name $a.name -Raw $Item.restore
    $after = Get-IemRegRaw -Path $a.path -Name $a.name
    if (-not (Test-IemRegRawSame -A $after -B $Item.restore)) { throw "$($Item.key): read back $($after.kind) after restoring $($Item.restore.kind)" }
    return [pscustomobject]@{ key = $Item.key; action = 'restored'; value = $Item.desired; error = $null }
}

function ConvertTo-IemMask {
    param([Parameter(Mandatory)][int[]]$Lps)
    [long]$m = 0
    foreach ($lp in $Lps) {
        if ($lp -lt 0 -or $lp -gt 62) { throw "logical processor $lp outside 0..62" }
        $m = $m -bor ([long]1 -shl $lp)
    }
    return $m
}

function ConvertTo-IemKaffinity {
    # A mask as the REG_BINARY KAFFINITY the Interrupt Affinity page describes
    # (design ref [11]): 8 bytes, little endian, as hex (M3).
    param([Parameter(Mandatory)][long]$Mask)
    return (@(for ($i = 0; $i -lt 8; $i++) { (($Mask -shr (8 * $i)) -band 0xFF).ToString('x2') }) -join '')
}

function Assert-IemHwidText {
    # A profile hardware id is compared exactly, so it must be a whole id: not
    # empty and without the -like wildcard characters * ? [ ] (M1).
    param([Parameter(Mandatory)][string]$What, [AllowNull()][AllowEmptyString()][string]$Hwid)
    if ([string]::IsNullOrWhiteSpace($Hwid)) { throw "${What}: the profile hwid is empty" }
    if ($Hwid -match '[\*\?\[\]]') { throw "${What}: the profile hwid '$Hwid' has a wildcard character" }
}

function Assert-IemDevice {
    # Before any write to a device: the profile's hwid is a PCI VEN_/DEV_ id and
    # equals (case-insensitive, exactly) one of the instance's HardwareID values.
    param([Parameter(Mandatory)]$Profile, [Parameter(Mandatory)]$Device)
    $what = "device $($Device.id)"
    Assert-IemHwidText -What $what -Hwid ([string]$Device.hwid)
    if ([string]$Device.hwid -notmatch '^PCI\\VEN_[0-9A-F]{4}&DEV_[0-9A-F]{4}(&|$)') { throw "${what}: the profile hwid '$($Device.hwid)' is not a PCI VEN_/DEV_ hardware id" }
    $key = Get-IemRegPath $Profile "HKLM:\SYSTEM\CurrentControlSet\Enum\$($Device.instance)"
    if (-not (Test-Path -LiteralPath $key)) { throw "${what}: instance not found (profile stale?)" }
    $hw = @((Get-Item -LiteralPath $key).GetValue('HardwareID', [string[]]@()))
    if (@($hw) -notcontains [string]$Device.hwid) { throw "${what}: hardware id does not match the profile" }
}

function Assert-IemDeviceLps {
    # Before an affinity write (review 3.9): the device names processors, each one
    # is present (group 0 of the CPU Set map), and the card's are exactly the
    # layout's card role.
    param([Parameter(Mandatory)]$Profile, [Parameter(Mandatory)]$Device)
    $lps = @(@($Device.lps) | ForEach-Object { [int]$_ } | Sort-Object)
    if ($lps.Count -eq 0) { throw "device $($Device.id): no processors (lps)" }
    $present = @([IemCpuSets]::Map().Keys)
    foreach ($lp in $lps) { if ($present -notcontains $lp) { throw "device $($Device.id): processor $lp is not present" } }
    if ([string]$Device.id -eq 'card') {
        $card = @(@($Profile.layout.card) | ForEach-Object { [int]$_ } | Sort-Object)
        if (($lps -join ',') -ne ($card -join ',')) { throw "device card: lps $($lps -join ',') differ from layout.card $($card -join ',')" }
    }
}

function Get-IemNicKey {
    # The NIC's driver key: nic.key (tests), else the Class key whose
    # NetCfgInstanceId is the adapter's, both under registry_root. -Check (before
    # any write) refuses unless the key's MatchingDeviceId, the hardware id its
    # driver matched, equals the profile's nic.hwid exactly, case-insensitive
    # (design note 7, A8, M1).
    param([Parameter(Mandatory)]$Profile, [switch]$Check)
    if ($Profile.nic.PSObject.Properties['key']) { $key = Get-IemRegPath $Profile $Profile.nic.key }
    else {
        $ad = @(Get-NetAdapter | Where-Object { $_.Name -eq $Profile.nic.adapter })
        if ($ad.Count -ne 1) { throw "adapter '$($Profile.nic.adapter)': $($ad.Count) adapters have this name" }
        $guid = "$($ad[0].InterfaceGuid)"
        $key = $null
        foreach ($k in @(Get-ChildItem -LiteralPath (Get-IemRegPath $Profile $script:NetClass) -ErrorAction SilentlyContinue)) {
            if ("$($k.GetValue('NetCfgInstanceId', ''))" -eq $guid) { $key = $k.PSPath; break }
        }
        if ($null -eq $key) { throw "adapter '$($Profile.nic.adapter)': driver key not found" }
    }
    if ($Check) {
        if (-not (Test-Path -LiteralPath $key)) { throw "nic: driver key not found (profile stale?)" }
        $hwid = $(if ($Profile.nic.PSObject.Properties['hwid']) { [string]$Profile.nic.hwid } else { '' })
        Assert-IemHwidText -What 'nic' -Hwid $hwid
        $matched = "$((Get-Item -LiteralPath $key).GetValue('MatchingDeviceId', ''))"
        if ([string]::IsNullOrEmpty($matched) -or $matched -ne $hwid) { throw "nic: hardware id does not match the profile (nic.hwid)" }
    }
    return $key
}

function Select-IemGroup {
    param([string[]]$Only, [Parameter(Mandatory)][string]$Group)
    return (@($Only).Count -eq 0) -or (@($Only) -contains $Group)
}

function Assert-IemOnly {
    # Every -Only name must be a group of the tier (irq:<id> for a profile
    # device, or one the journal holds for undo): a typo would otherwise apply
    # or revert nothing and still succeed (A9).
    param([Parameter(Mandatory)]$Profile, [Parameter(Mandatory)][int]$Tier, [string[]]$Only = @(), [string[]]$Also = @())
    $known = @('services', 'updates', 'maintenance', 'defender')
    if ($Tier -eq 3) { $known = @('irq', 'nic') + @(@($Profile.devices) | ForEach-Object { "irq:$($_.id)" }) }
    $known = @($known) + @($Also)
    $bad = @(@($Only) | Where-Object { $known -notcontains $_ })
    if ($bad.Count -gt 0) { throw "-Only $($bad -join ', '): no tier $Tier group of that name (groups: $(@($known | Sort-Object -Unique) -join ', '))" }
}

function Get-IemGlobalItems {
    # Tier 2 (no reboot) and Tier 3 (reboot) items of the profile. -Check
    # verifies each device before its items are built (apply only).
    param([Parameter(Mandatory)]$Profile, [Parameter(Mandatory)][int]$Tier, [string[]]$Only = @(), [switch]$Check)
    $items = @()
    $granted = $null   # instance id -> granted IRQs, read once when a write needs it
    if ($Tier -eq 2) {
        if (Select-IemGroup $Only 'services') {
            foreach ($n in @($Profile.services_disable)) {
                # Stopped first, then disabled: undo runs newest first, so the start
                # type is restored before the service is started again.
                $items += New-IemItem -Key "svc:${n}:state" -Kind 'svc-state' -Arguments @{ name = $n } -Desired 'stopped' -Tier 2 -Group 'services'
                $items += New-IemItem -Key "svc:${n}:start" -Kind 'svc-start' -Arguments @{ name = $n } -Desired 'disabled' -Tier 2 -Group 'services'
            }
        }
        if (Select-IemGroup $Only 'updates') {
            foreach ($n in @($Profile.updates.services)) {
                # Stopped first, then disabled: undo runs newest first, so the start
                # type is restored before the service is started again.
                $items += New-IemItem -Key "svc:${n}:state" -Kind 'svc-state' -Arguments @{ name = $n } -Desired 'stopped' -Tier 2 -Group 'updates'
                $items += New-IemItem -Key "svc:${n}:start" -Kind 'svc-start' -Arguments @{ name = $n } -Desired 'disabled' -Tier 2 -Group 'updates'
            }
            foreach ($t in @($Profile.updates.tasks)) { $items += New-IemTaskItem -Task $t -Group 'updates' }
        }
        if (Select-IemGroup $Only 'maintenance') {
            if ($Profile.maintenance.off) {
                $items += New-IemItem -Key 'reg:maintenance-disabled' -Kind 'reg' -Tier 2 -Group 'maintenance' -Desired 1 `
                    -Arguments @{ path = (Get-IemRegPath $Profile 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Schedule\Maintenance'); name = 'MaintenanceDisabled'; type = 'DWord' }
            }
            foreach ($t in @($Profile.maintenance.tasks)) { $items += New-IemTaskItem -Task $t -Group 'maintenance' }
        }
        if (Select-IemGroup $Only 'defender') {
            foreach ($p in @($Profile.defender.paths)) { $items += New-IemItem -Key "defender:path:$p" -Kind 'defender-path' -Arguments @{ value = $p } -Desired 'present' -Tier 2 -Group 'defender' }
            foreach ($p in @($Profile.defender.processes)) { $items += New-IemItem -Key "defender:process:$p" -Kind 'defender-process' -Arguments @{ value = $p } -Desired 'present' -Tier 2 -Group 'defender' }
        }
    } elseif ($Tier -eq 3) {
        foreach ($d in @($Profile.devices)) {
            $wanted = (@($Only) -contains "irq:$($d.id)") -or ([bool]$d.enabled -and (Select-IemGroup $Only 'irq'))
            if (-not $wanted) { continue }
            if ($Check) { Assert-IemDevice -Profile $Profile -Device $d; Assert-IemDeviceLps -Profile $Profile -Device $d }
            $im = "HKLM:\SYSTEM\CurrentControlSet\Enum\$($d.instance)\Device Parameters\Interrupt Management"
            # The affinity applies only while the device already uses MSI (design note
            # 6.4 R1); enabling MSI is the owner's Tier 4 decision X3 (A7). MSI is in use
            # when the driver's MSISupported flag is 1 AND the interrupts the device holds
            # now are message-signaled (m3); the grant is read only for a write (-Check).
            $msi = Get-IemRegRaw -Path (Get-IemRegPath $Profile "$im\MessageSignaledInterruptProperties") -Name 'MSISupported'
            $why = $null
            if (-not ($msi.kind -eq 'DWord' -and $msi.data -eq '1')) { $why = 'uses line-based interrupts (MSISupported is not 1)' }
            elseif ($Check) {
                if ($null -eq $granted) { $granted = & $script:ReadAllocatedIrqs }
                $irqs = @()
                if ($granted.ContainsKey([string]$d.instance)) { $irqs = @($granted[[string]$d.instance]) }
                if ($irqs.Count -eq 0) { $why = 'has no interrupt granted now (MSI not confirmed)' }
                elseif (@($irqs | Where-Object { [int]$_ -ge 0 }).Count -gt 0) { $why = 'uses line-based interrupts (INTx granted although MSISupported is 1)' }
            }
            if ($why) {
                $why = "skipped: $($d.id) $why"
                $items += New-IemItem -Key "irq:$($d.id)" -Kind 'skip' -Arguments @{ reason = $why } -Desired $why -Tier 3 -Group "irq:$($d.id)"
                continue
            }
            $key = Get-IemRegPath $Profile "$im\Affinity Policy"
            $items += New-IemItem -Key "irq:$($d.id):policy" -Kind 'reg' -Arguments @{ path = $key; name = 'DevicePolicy'; type = 'DWord' } -Desired 4 -Tier 3 -Group "irq:$($d.id)" -Reboot
            # REG_BINARY, the KAFFINITY's canonical form (M3); read back byte for byte.
            $items += New-IemItem -Key "irq:$($d.id):mask" -Kind 'reg' -Arguments @{ path = $key; name = 'AssignmentSetOverride'; type = 'Binary' } `
                -Desired (ConvertTo-IemKaffinity -Mask (ConvertTo-IemMask @($d.lps))) -Tier 3 -Group "irq:$($d.id)" -Reboot
        }
        if (Select-IemGroup $Only 'nic') {
            $nk = Get-IemNicKey -Profile $Profile -Check:$Check
            foreach ($p in $Profile.nic.properties.PSObject.Properties) {
                $items += New-IemItem -Key "nic:$($p.Name)" -Kind 'reg' -Arguments @{ path = $nk; name = $p.Name; type = 'String' } -Desired $p.Value -Tier 3 -Group 'nic' -Reboot
            }
            $items += New-IemItem -Key 'nic:*RssBaseProcNumber' -Kind 'reg' -Arguments @{ path = $nk; name = '*RssBaseProcNumber'; type = 'String' } -Desired $Profile.nic.rss.base -Tier 3 -Group 'nic' -Reboot
            $items += New-IemItem -Key 'nic:*RssMaxProcNumber' -Kind 'reg' -Arguments @{ path = $nk; name = '*RssMaxProcNumber'; type = 'String' } -Desired $Profile.nic.rss.max -Tier 3 -Group 'nic' -Reboot
            $items += New-IemItem -Key 'nic:PnPCapabilities' -Kind 'reg' -Arguments @{ path = $nk; name = 'PnPCapabilities'; type = 'DWord' } -Desired $Profile.nic.pnp_capabilities -Tier 3 -Group 'nic' -Reboot
        }
    } else { throw "tier $Tier refused (2 or 3)" }
    return ,$items
}

function New-IemTaskItem {
    param([Parameter(Mandatory)][string]$Task, [Parameter(Mandatory)][string]$Group)
    $i = $Task.LastIndexOf('\')
    New-IemItem -Key "task:$Task" -Kind 'task' -Arguments @{ path = $Task.Substring(0, $i + 1); name = $Task.Substring($i + 1) } -Desired 'disabled' -Tier 2 -Group $Group
}

# The plan-name reader: module-private, so the self-test can replace it and no
# caller can bypass it (review 3.1).
$script:ReadPlanName = { param([string]$Guid) [IemPower]::Name($Guid) }

function Get-IemPlanName {
    # The plan's friendly name, or $null only when the plan does not exist: a name
    # that cannot be read for an existing plan throws, never reads as "no plan".
    param([Parameter(Mandatory)][string]$Guid)
    try { $name = & $script:ReadPlanName $Guid }
    catch {
        if (Test-IemPlan -Guid $Guid) { throw "plan ${Guid}: exists, but its name cannot be read ($_)" }
        return $null
    }
    if ($null -eq $name -and (Test-IemPlan -Guid $Guid)) { throw "plan ${Guid}: exists, but its name cannot be read" }
    return $name
}

function Assert-IemOwnPlan {
    # Plan values are written only into iemmixer's own plan (M2): never the
    # REAPER-mode plan it duplicates (plan.source), never a built-in scheme, and an
    # existing plan only when it carries iemmixer's name.
    param([Parameter(Mandatory)]$Profile)
    $g = ([guid][string]$Profile.plan.guid).ToString()
    $s = ([guid][string]$Profile.plan.source).ToString()
    if ($g -eq $s) { throw "plan.guid $g is plan.source, the REAPER-mode plan: iemmixer never writes into it" }
    if ($script:BuiltinSchemes -contains $g) { throw "plan.guid $g is a built-in Windows scheme: iemmixer writes only into its own plan" }
    $name = Get-IemPlanName -Guid $g
    if ($null -ne $name -and $name -cne $script:PlanName) { throw "plan $g exists and is not iemmixer's (named '$name'): nothing written" }
}

function Get-IemModeItems {
    # Mode levers (design note 6.2) in apply order: L3 governor, L2 plan, L6 services,
    # L4 placement. The governor is paused first, so Process Lasso no longer owns the
    # power plan when the iemmixer plan activates (A5); exit restores it last.
    param([Parameter(Mandatory)]$Profile, [string[]]$Only = @('plan', 'governor', 'placement'), [ValidateSet('default', 'c1', 'disable')][string]$Idle = 'default')
    $items = @()
    $guid = $Profile.plan.guid
    if (@($Only) -contains 'governor') {
        $items += New-IemItem -Key 'governor' -Kind 'svc-state' -Arguments @{ name = $Profile.governor } -Desired 'stopped' -Group 'governor'
    }
    if (@($Only) -contains 'plan') {
        Assert-IemOwnPlan -Profile $Profile
        # The plan and its settings are ensured, not journaled: exit re-activates the
        # journaled plan and leaves this one defined but inactive; enter reuses it (A6).
        $items += New-IemItem -Key 'plan:exists' -Kind 'plan-exists' -Arguments @{ guid = $guid; source = $Profile.plan.source } -Desired 'present' -Group 'plan' -NoJournal
        $values = @($script:PlanSettings) + @(
            @{ name = 'idle-disable'; sub = $script:ProcessorSub; setting = $script:IdleDisable; value = $(if ($Idle -eq 'disable') { 1 } else { 0 }) },
            @{ name = 'idle-state-max'; sub = $script:ProcessorSub; setting = $script:IdleStateMax; value = $(if ($Idle -eq 'c1') { 1 } else { 0 }) })
        foreach ($s in $values) {
            $items += New-IemItem -Key "plan:$($s.name)" -Kind 'plan-value' -Arguments @{ guid = $guid; sub = $s.sub; setting = $s.setting } -Desired $s.value -Group 'plan' -NoJournal
        }
        $items += New-IemItem -Key 'plan:active' -Kind 'plan-active' -Arguments @{} -Desired $guid -Group 'plan'
    }
    if (@($Only) -contains 'services') {
        foreach ($n in @($Profile.services_mode)) { $items += New-IemItem -Key "mode-svc:$n" -Kind 'svc-state' -Arguments @{ name = $n } -Desired 'stopped' -Group 'services' }
    }
    if (@($Only) -contains 'placement') {
        $map = [IemCpuSets]::Map()
        $ids = @(@($Profile.layout.housekeeping) | ForEach-Object { $map[[int]$_] }) | Sort-Object
        foreach ($name in @($Profile.placement)) {
            foreach ($p in @(Get-Process -Name $name -ErrorAction SilentlyContinue)) {
                $items += New-IemItem -Key "placement:${name}:$($p.Id)" -Kind 'cpusets' -Group 'placement' -Desired ($ids -join ',') `
                    -Arguments @{ pid = $p.Id; name = $p.ProcessName; start = $p.StartTime.ToUniversalTime().Ticks }
            }
        }
    }
    return ,$items
}

function Invoke-IemTuningApply {
    param([Parameter(Mandatory)][string]$ProfilePath, [Parameter(Mandatory)][ValidateSet(2, 3)][int]$Tier, [string[]]$Only = @())
    $profile = Read-IemProfile -Path $ProfilePath
    Assert-IemLayout -Profile $profile
    Assert-IemOnly -Profile $profile -Tier $Tier -Only $Only
    $j = Read-IemJournal -Path $profile.journal
    $boot = Get-IemBootIdentity
    $rows = @(foreach ($item in (Get-IemGlobalItems -Profile $profile -Tier $Tier -Only $Only -Check)) {
        Invoke-IemItem -Item $item -Journal $j -Section 'global' -Path $profile.journal -Boot $boot
    })
    # The journal names the profile version of THIS tier only after its complete
    # apply without a failed row: a partial -Only or a failure keeps the drift (A9, m2).
    if (@($Only).Count -eq 0 -and @($rows | Where-Object { $_.action -eq 'failed' }).Count -eq 0) {
        $j.applied["tier$Tier"] = [int]$profile.version
        Write-IemJournal -Path $profile.journal -Journal $j
    }
    return ,$rows
}

function Undo-IemTuning {
    param([Parameter(Mandatory)][string]$ProfilePath, [Parameter(Mandatory)][ValidateSet(2, 3)][int]$Tier, [string[]]$Only = @())
    $profile = Read-IemProfile -Path $ProfilePath
    $j = Read-IemJournal -Path $profile.journal
    $held = @(foreach ($k in @($j.order.global)) { $e = $j.global[$k]; if ($null -ne $e -and [int]$e.tier -eq $Tier) { [string]$e.group } })
    Assert-IemOnly -Profile $profile -Tier $Tier -Only $Only -Also $held
    $boot = Get-IemBootIdentity
    $keys = @($j.order.global); [array]::Reverse($keys)
    $rows = @()
    foreach ($k in $keys) {
        $e = $j.global[$k]
        if ($null -eq $e -or [int]$e.tier -ne $Tier) { continue }
        $g = [string]$e.group
        if (@($Only).Count -gt 0 -and -not ((@($Only) -contains $g) -or (@($Only) -contains ($g -replace ':.*$', '')))) { continue }
        try {
            $rows += Restore-IemItem -Item (ConvertTo-IemItem -Key $k -Entry $e)
            if ([bool]$e.reboot) { $j.reverted[$k] = $boot }
            $j.global.Remove($k)
            $j.order.global = @($j.order.global | Where-Object { $_ -ne $k })
            Write-IemJournal -Path $profile.journal -Journal $j
        } catch { $rows += [pscustomobject]@{ key = $k; action = 'failed'; value = $null; error = "$_" } }
    }
    return ,$rows
}

function Enter-IemTuningMode {
    param([Parameter(Mandatory)][string]$ProfilePath, [string[]]$Only = @('plan', 'governor', 'placement'),
          [ValidateSet('default', 'c1', 'disable')][string]$Idle = 'default')
    $profile = Read-IemProfile -Path $ProfilePath
    Assert-IemLayout -Profile $profile
    # Built, and so checked (M2), before anything is written.
    $items = Get-IemModeItems -Profile $profile -Only $Only -Idle $Idle
    $j = Read-IemJournal -Path $profile.journal
    $j.entered = $true
    Write-IemJournal -Path $profile.journal -Journal $j   # before any write: an exit after a crash finds it
    $boot = Get-IemBootIdentity
    $planWritten = $false
    $rows = @(foreach ($item in $items) {
        $row = Invoke-IemItem -Item $item -Journal $j -Section 'mode' -Path $profile.journal -Boot $boot
        if ($item.kind -eq 'plan-value' -and $row.action -eq 'written') { $planWritten = $true }
        if ($item.kind -eq 'plan-active' -and $row.action -eq 'kept' -and $planWritten) {
            # Values written into the active plan take effect only through
            # PowerSetActiveScheme (PowerWriteACValueIndex docs), so re-activate it (A4).
            try { [IemPower]::Activate([string]$item.desired); $row.action = 'reactivated' } catch { $row.action = 'failed'; $row.error = "$_" }
        }
        $row
    })
    return ,$rows
}

function Exit-IemTuningMode {
    # Restores every mode item from the journal alone (newest first), carrying
    # on past failures; throws at the end when anything could not be restored.
    param([Parameter(Mandatory)][string]$ProfilePath)
    $profile = Read-IemProfile -Path $ProfilePath
    # Only the mode section: a global-section problem never blocks the exit ("ide
    # event", logon); it is reported with the rows (review 3.2).
    $j = Read-IemJournal -Path $profile.journal -ModeOnly
    $keys = @($j.order.mode); [array]::Reverse($keys)
    # The governor restarts only after the REAPER-mode plan is active again (A5),
    # whatever order partial enters journaled the items in.
    $gov = @($keys | Where-Object { $g = $j.mode[$_]; $null -ne $g -and [string]$g.group -eq 'governor' })
    $keys = @(@($keys | Where-Object { $gov -notcontains $_ }) + $gov)
    $rows = @(); $failed = @()
    foreach ($k in $keys) {
        $e = $j.mode[$k]
        if ($null -eq $e) { continue }
        try {
            $rows += Restore-IemItem -Item (ConvertTo-IemItem -Key $k -Entry $e)
            $j.mode.Remove($k)
            $j.order.mode = @($j.order.mode | Where-Object { $_ -ne $k })
            Write-IemJournal -Path $profile.journal -Journal $j
        } catch { $failed += "${k}: $_" }
    }
    if ($failed.Count -gt 0) {
        $all = @($failed) + @(@($j.problems) | ForEach-Object { "journal problem: $($_.text)" })
        throw ("tuning exit left $($failed.Count) item(s): " + ($all -join '; '))
    }
    $j.entered = $false
    Write-IemJournal -Path $profile.journal -Journal $j
    foreach ($p in @($j.problems)) { $rows += [pscustomobject]@{ key = 'journal'; action = 'problem'; value = $null; error = $p.text } }
    return ,$rows
}

function Get-IemTuningState {
    param([Parameter(Mandatory)][string]$ProfilePath)
    $profile = Read-IemProfile -Path $ProfilePath
    $j = Read-IemJournal -Path $profile.journal
    $boot = Get-IemBootIdentity
    $rows = @()
    foreach ($tier in 2, 3) {
        foreach ($item in (Get-IemGlobalItems -Profile $profile -Tier $tier)) {
            $e = $j.global[$item.key]
            $actual = try { Get-IemValue -Item $item } catch { "error: $_" }
            $rows += [pscustomobject]@{
                key = $item.key; tier = $tier; group = $item.group; desired = $item.desired; actual = $actual
                ok = (Test-IemSame $actual $item.desired); journaled = [bool]$e; before = $(if ($e) { $e.before } else { $null })
                pending = [bool]($e -and $item.reboot -and (Test-IemSameBoot -A $e.boot -B $boot))
                revert_pending = [bool]($j.reverted.ContainsKey($item.key) -and (Test-IemSameBoot -A $j.reverted[$item.key] -B $boot))
            }
        }
    }
    # A tier drifts when the journal holds its items and its last complete apply
    # was of another profile version (m2).
    $driftTiers = @(foreach ($tier in 2, 3) {
        $held = @($j.global.Values | Where-Object { [int]$_.tier -eq $tier }).Count -gt 0
        if ($held -and [int]$j.applied["tier$tier"] -ne [int]$profile.version) { $tier }
    })
    [pscustomobject]@{
        version = [int]$profile.version; applied_version = [pscustomobject]@{ tier2 = $j.applied.tier2; tier3 = $j.applied.tier3 }
        boot = $boot.time; boot_id = $boot.id
        drift = [bool]($driftTiers.Count -gt 0); drift_tiers = $driftTiers
        entered = $j.entered; mode_items = @($j.order.mode); items = $rows
    }
}

# The interrupt-grant reader: module-private, so the self-test can replace it and
# no caller can skip the read that gates the card's affinity (review 3.5).
$script:ReadAllocatedIrqs = { Get-IemAllocatedIrqs }

function Get-IemAllocatedIrqs {
    # Instance id -> the IRQ numbers Windows granted the device now, signed: a
    # negative number is a message-signaled interrupt (Win32_PnPAllocatedResource).
    $irq = @{}
    foreach ($r in @(Get-CimInstance -ClassName Win32_PnPAllocatedResource -ErrorAction Stop)) {
        if ($r.Antecedent.CimSystemProperties.ClassName -ne 'Win32_IRQResource') { continue }
        $id = [string]$r.Dependent.DeviceID
        $n = [BitConverter]::ToInt32([BitConverter]::GetBytes([uint32]$r.Antecedent.IRQNumber), 0)
        if (-not $irq.ContainsKey($id)) { $irq[$id] = @() }
        $irq[$id] = @($irq[$id]) + $n
    }
    return $irq
}

Export-ModuleMember -Function *-Iem*
