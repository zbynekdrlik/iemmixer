#Requires -Version 5.1
# HIL v1's helpers (hil-v1.ps1, design note section 7; split out of
# IemPc.psm1 by responsibility, #36): iemmode's replies, the checks and the
# public summary. Pure, except Invoke-IemMode (runs iemmode) and Get-IemJson
# (one HTTP GET).
# IemPc.psm1 imports this module from its own folder (-Global, as IemTuning
# imports IemTuningStore, #34), so it goes wherever IemPc.psm1 goes: the
# bundle, the bootstrap's admin-only stage, <elevated root>\tasks and the
# cutover's and the ssh shell undo's folders. Its functions reach the other
# IemPc modules' through the global session state. Nothing is ended by force
# (I8). No site value lives here (P6).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

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
    # The same rule: iem_guard::effects::web::version_matches and iem-soakclient's names_build.
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
