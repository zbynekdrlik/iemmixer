#Requires -Version 5.1
# The driver's preference and the driver module's holders (I2, #9
# 2026-09-28; split out of IemPc.psm1 by responsibility, #36): the value read
# and its kind, the pure decision (Get-IemPrefAction) and the logon task's
# Restore-IemPref, which never writes while a process holds the module.
# IemPc.psm1 imports this module from its own folder (-Global, as IemTuning
# imports IemTuningStore, #34), so it goes wherever IemPc.psm1 goes: the
# bundle, the bootstrap's admin-only stage, <elevated root>\tasks and the
# cutover's and the ssh shell undo's folders. Its functions reach the other
# IemPc modules' through the global session state. Nothing is ended by force
# (I8). No site value lives here (P6).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# A driver module's name, as the [card] module names it (no path).
$script:ModulePattern = '^[^\\/:*?"<>|]+\.dll$'

# ---- the preference and its holders (I2) ----

function Get-IemModuleHolders {
    # Processes that have the driver module loaded, as "image:pid" (I3).
    param([Parameter(Mandatory)][string]$Module)
    if ($Module -cnotmatch $script:ModulePattern) { throw "module name '$Module' refused" }
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

function Get-IemPrefAction {
    # The preference check's decision, the guard's PrefCheck rule (design
    # section 5.2 step 5; #9 2026-09-28): the original -> 'none'; not the
    # original and no process holds the driver module -> 'restore'; not the
    # original while any process holds it -> 'held', never written (its driver
    # would most likely ask it for a reset: a REAPER at an event would drop
    # out). -Holders: the module's holders as read ("image:pid"), $null when
    # they could not be read; then a running REAPER (-Reaper, "image:pid") is
    # assumed to hold it, as the guard assumes. Returns the action and the
    # holders it went by.
    param([Parameter(Mandatory)][bool]$IsOriginal, $Holders = $null, [string[]]$Reaper = @())
    if ($IsOriginal) { return [pscustomobject]@{ action = 'none'; holders = @() } }
    $seen = @($Reaper)
    if ($null -ne $Holders) { $seen = @($Holders) }
    if ($seen.Count -gt 0) { return [pscustomobject]@{ action = 'held'; holders = $seen } }
    return [pscustomobject]@{ action = 'restore'; holders = @() }
}

function Restore-IemPref {
    # The preferred buffer back to its recorded original by the guard's
    # PrefCheck rule (Get-IemPrefAction; design section 5.2 step 5; G1 at
    # logon; #9 2026-09-28): at the original nothing is read or written;
    # otherwise the driver module's holders are read, and only while none
    # holds it is the original written (kind kept, each write read back, three
    # attempts at most). While one holds it nothing is written: 'held', with
    # the value and the holders. An unreadable holder list assumes a running
    # REAPER (-ReaperImage) holds it.
    param([Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][string]$Original,
          [Parameter(Mandatory)][string]$Module, [string]$ReaperImage = 'reaper')
    if ($Original -cnotmatch '^[0-9]{1,5}$') { throw "original '$Original' refused (digits)" }
    if ($Module -cnotmatch $script:ModulePattern) { throw "module name '$Module' refused" }
    $path = ConvertTo-IemHkcuPath -Key $Key
    $before = Get-IemPref -Key $Key -Name $Name
    $isOriginal = Test-IemPrefIsOriginal -Pref $before -Original $Original
    $holders = $null
    $reaper = @()
    $holdersError = ''
    if (-not $isOriginal) {
        try { $holders = Get-IemModuleHolders -Module $Module } catch {
            $holdersError = $_.Exception.Message
            $reaper = @(Get-Process -Name $ReaperImage -ErrorAction SilentlyContinue | ForEach-Object { '{0}:{1}' -f $_.ProcessName, $_.Id })
        }
    }
    $decision = Get-IemPrefAction -IsOriginal $isOriginal -Holders $holders -Reaper $reaper
    $now = $before
    $attempts = 0
    if ($decision.action -eq 'restore') {
        while (-not (Test-IemPrefIsOriginal -Pref $now -Original $Original) -and $attempts -lt 3) {
            $attempts++
            $data = [int]$Original
            if ($now.kind -eq 'String') { $data = $Original }
            Set-ItemProperty -LiteralPath $path -Name $Name -Value $data -Type $now.kind
            $now = Get-IemPref -Key $Key -Name $Name
        }
    }
    [pscustomobject]@{ before = $before.raw; after = $now.raw; kind = $now.kind; attempts = $attempts
                       action = $decision.action; holders = @($decision.holders); holders_error = $holdersError
                       ok = (Test-IemPrefIsOriginal -Pref $now -Original $Original) }
}

function Get-IemPrefKind {
    # The preference's registry kind as the boot task writes it (dword or
    # text), read from the value in this session's HKCU (Get-IemPref: DWord or
    # String, else refused). Register-IemTasks runs as the desktop user, as
    # `iempc bootstrap` does (the tasks' user by default), so this is the
    # driver's value; a missing key or value is refused (a typo would make
    # reg.exe add create one).
    param([Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][string]$Name)
    try { $p = Get-IemPref -Key $Key -Name $Name } catch {
        throw "the preference $Name under $Key cannot be read in this session's HKCU (run Register-IemTasks as the desktop user): $($_.Exception.Message)"
    }
    if ($p.kind -eq 'String') { return 'text' }
    return 'dword'
}

Export-ModuleMember -Function *-Iem*
