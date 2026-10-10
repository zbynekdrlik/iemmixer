#Requires -Version 5.1
<#
HIL v1 (S6 design note section 7). The ops repo's hil.yml `pc` job runs it from the
verified bundle directory (bundles\<sha>\) after `iemmode install`:

  hil-v1.ps1 -Sha <40 hex> -Branch dev|main -JobRun <run id> -Out <result.json>

Public, with no site value: it reads everything through `iemmode` and the
local server's /api/site, and never talks to GitHub.

Order: `iemmode job-begin` (the guard refuses unless dev and no other job
runs; a switch refuses it; no stage reading, #38) -> `iemmode activate` -> the checks
-> `iemmode job-end` -> `iemmode report <sha> green|red <summary>` ->
result.json {conclusion, summary, why, checks}. Once a switch to event started
(the guard left dev or runs a switch) the job ends as cancelled, never success,
and reports no result; a refused job-begin is cancelled too (the PC is not
free). The summary is public (the ops report job posts it as the hil/iem-pc
check run, P6): check names, counts and fixed cancel phrases only; the guard's
own text goes to `why` and the checks' details, both private.
Exit 0 for success or cancelled, 1 for failure (the report job posts the
conclusion from result.json).

The engine checks read `engine` from the `iemmode status` reply (the guard's
Reply.engine): build (the bundle SHA), frames (measured), callbacks, missed,
resets, parked, faulted, pipe_private (the engine pipes' DACL holds only the
user and SYSTEM), spawns (engines the guard started), last_exit and hil (each
of HIL's spare card outputs, [guard] hil_tx, with its peak since the engine's
previous Status: the test signal's level during its TTL, silence after). A check
whose data is missing fails. While an engine comes up the guard shows none
(until its hello and first Status; after a hand-over to a new guard exe, until
that guard looked at it), so after activate, a forced reopen and the respawn
the script polls `iemmode status` for at most -EngineWait seconds until the
engine it expects shows (Test-IemHilEngineUp), then the check judges the last
status. The panic check drives `iemmode inject-fault` and wants exit 70,
exactly one respawn within -PanicWait seconds and the new engine streaming at
32.

HIL v2 (S7, #10, plan Task 31; the checks are IemHil.psm1's, imported from
this folder): `pipe-owner` (the engine's pid serves its control pipe,
Reply.engine.pipe_server_pid, and the pipes are private), `tunnel-peer` and
`lan-peer` (/api/peer through the public host and on the LAN, as /api/site
names them), `reopen-time` (Reply.engine.last_reopen_us of the forced reopen
within -ReopenMaxMs), `fault-time` (Reply.engine.last_fault_us changed by the
injection and under -FaultMaxUs), `alarm-ack` (once alarm-push passed: its own
test alarm, the one new above the highest id read before alarm-test, then
earlier runs' unacknowledged ones; only the guard's exact test text with no
step and no owner question) and F30's bytes. A field an older guard or engine
does not send fails its check ("lacks '<field>'"). F30 runs when -SiteChange,
-SiteRevert and -SiteInstalled (the installed site's path) are given together
(none: no f30 check at all): nothing is installed unless the revert file is the
installed site byte for byte and the change file is not; then the installed
site's sha256 after the change must differ and after the revert must equal the
one before. A cancel at the revert names the change left installed in `why`.
#>
param(
    [string]$Sha = '',
    [string]$Branch = '',
    [string]$JobRun = '',
    [string]$Out = '',
    [string]$Iemmode = '',
    [string]$Local = 'http://127.0.0.1',
    [int]$CardSeconds = 120,
    [string]$TestInput = 'mic1',
    [double]$TestDbfs = -30,
    [double]$TestTtl = 10,
    [double]$PanicWait = 30,
    [double]$EngineWait = 30,
    [string]$SiteChange = '',
    [string]$SiteRevert = '',
    [string]$SiteInstalled = '',
    # S1a's reopen, about 104 ms at 64 samples, doubled; tighten after the first HIL v2 run (Task 33).
    [double]$ReopenMaxMs = 150,
    [double]$FaultMaxUs = 1000
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'IemPc.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'IemHil.psm1') -Force

if (-not $Iemmode) { $Iemmode = Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) 'bin\iemmode.exe' }
$script:checks = New-Object System.Collections.ArrayList
$script:cancelled = $false
# A cancel's reason code (the public summary's fixed phrase) and its text (result.json `why`, private).
$script:reason = ''
$script:why = ''
$inv = [Globalization.CultureInfo]::InvariantCulture

function Add-HilCheck {
    param([Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][bool]$Ok, [string]$Detail = '', $Numbers = $null)
    [void]$script:checks.Add((New-IemHilCheck -Name $Name -Ok $Ok -Detail $Detail -Numbers $Numbers))
}

function Invoke-Hil {
    # One iemmode call; a refusal once a switch to event started cancels the job.
    param([Parameter(Mandatory)][string[]]$A)
    $r = Invoke-IemMode -Exe $Iemmode -Arguments $A
    if (-not (Test-IemModeOk -Result $r) -and (Test-IemHilSwitchStarted -Reply $r.reply)) {
        $script:cancelled = $true
        $script:reason = 'left-dev'
        $script:why = ('iemmode {0}: the guard left dev ({1})' -f $A[0], (Get-IemModeText -Result $r))
    }
    return $r
}

function Get-HilStatus {
    # The guard's status reply, or $null; a reply outside dev cancels the job.
    $r = Invoke-Hil -A @('status')
    if (-not (Test-IemModeOk -Result $r)) { return $null }
    if (Test-IemHilSwitchStarted -Reply $r.reply) {
        $script:cancelled = $true
        $script:reason = 'left-dev'
        $script:why = 'status: the guard left dev (a switch to event started)'
        return $null
    }
    return $r.reply
}

function Wait-HilEngine {
    # Polls `iemmode status` every 500 ms for at most -EngineWait seconds until the guard
    # shows the engine a check expects (Test-IemHilEngineUp: frames 32 and callbacks above
    # -Callbacks; with -Build this bundle's build; with -Resets more resets than that).
    # Returns the last status (the check judges it), or $null once the job is cancelled.
    param([int64]$Callbacks = 0, [switch]$Build, $Resets = $null)
    $want = ''
    if ($Build) { $want = $Sha }
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while ($true) {
        $st = Get-HilStatus
        if ($script:cancelled) { return $null }
        if (Test-IemHilEngineUp -Engine (Get-IemProp $st 'engine') -Callbacks $Callbacks -Sha $want -Resets $Resets) { return $st }
        if ($clock.Elapsed.TotalSeconds -ge $EngineWait) { return $st }
        Start-Sleep -Milliseconds 500
    }
}

function Wait-HilSilence {
    # Polls `iemmode status` every 500 ms from the test signal's start: until its -Ttl ran
    # out and then, for at most -EngineWait seconds more, until the engine shows every HIL
    # spare output silent (Test-IemHilSilent) after a status that carried the signal
    # (Test-IemHilHeard). A silent status before that proves nothing (it may cover the time
    # before the signal). Every status read until then is added to $Seen: the engine's
    # Status carries the peaks since its previous one (about once a second), so a short
    # TTL's signal may show only in a status that arrives after the TTL. Returns the last
    # status (the check judges it), or $null once the job is cancelled.
    param([Parameter(Mandatory)][AllowEmptyCollection()][System.Collections.ArrayList]$Seen, [Parameter(Mandatory)][double]$Ttl)
    $heard = $false
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while ($true) {
        $st = Get-HilStatus
        if ($script:cancelled) { return $null }
        $engine = Get-IemProp $st 'engine'
        $t = $clock.Elapsed.TotalSeconds
        if ($heard -and $t -ge $Ttl -and (Test-IemHilSilent -Engine $engine)) { return $st }
        [void]$Seen.Add($engine)
        if (Test-IemHilHeard -Engine $engine) { $heard = $true }
        if ($t -ge $Ttl + $EngineWait) { return $st }
        Start-Sleep -Milliseconds 500
    }
}

function Invoke-HilUrlCheck {
    # An address the server names answers /api/version with this bundle.
    param([Parameter(Mandatory)][string]$Name, [string]$Base = '')
    if (-not $Base) { Add-HilCheck $Name $false 'the server names no such address (/api/site)'; return }
    try {
        $v = Test-IemHilVersion -Sha $Sha -Version (Get-IemJson -Uri ($Base.TrimEnd('/') + '/api/version'))
        Add-HilCheck $Name $v.ok ('{0}: {1}' -f $Base, $v.detail)
    } catch {
        Add-HilCheck $Name $false ('{0}/api/version: {1}' -f $Base, $_.Exception.Message)
    }
}

function Invoke-HilPeerCheck {
    # How the server classified a request to an address it names (/api/peer, fixed codes).
    param([Parameter(Mandatory)][string]$Name, [string]$Base = '', [Parameter(Mandatory)][string]$Want)
    if (-not $Base) { Add-HilCheck $Name $false 'the server names no such address (/api/site)'; return }
    try {
        $p = Test-IemHilPeer -Peer (Get-IemJson -Uri ($Base.TrimEnd('/') + '/api/peer')) -Want $Want
        Add-HilCheck $Name $p.ok ('{0}: {1}' -f $Base, $p.detail) $p.numbers
    } catch {
        Add-HilCheck $Name $false ('{0}/api/peer: {1}' -f $Base, $_.Exception.Message)
    }
}

function Invoke-HilChecks {
    $r = Invoke-Hil -A @('activate', $Sha)
    if ($script:cancelled) { return }
    if (-not (Test-IemModeOk -Result $r)) { Add-HilCheck 'activate' $false (Get-IemModeText -Result $r); return }
    Add-HilCheck 'activate' $true ('bundle {0} active' -f $Sha)

    # Versions: the engine's build and the server's /api/version name this SHA. The
    # guard shows the new engine once it said hello and sent a Status (after a hand-over,
    # once the new guard exe looked at it): wait for this build streaming at 32.
    $st = Wait-HilEngine -Build
    if ($script:cancelled) { return }
    $build = [string](Get-IemProp (Get-IemProp $st 'engine') 'build')
    Add-HilCheck 'engine-build' ($build -ceq $Sha) ("engine build '{0}'" -f $build)
    try {
        $v = Test-IemHilVersion -Sha $Sha -Version (Get-IemJson -Uri ($Local + '/api/version'))
        Add-HilCheck 'server-version' $v.ok $v.detail
    } catch {
        Add-HilCheck 'server-version' $false ('{0}/api/version: {1}' -f $Local, $_.Exception.Message)
    }

    # LAN and the public host (the tunnel peer), as the server names them.
    $site = $null
    try { $site = Get-IemJson -Uri ($Local + '/api/site') } catch {
        Add-HilCheck 'site-links' $false ('{0}/api/site: {1}' -f $Local, $_.Exception.Message)
    }
    $lanUrl = [string](Get-IemProp $site 'lan_url')
    Invoke-HilUrlCheck -Name 'lan' -Base $lanUrl
    $public = [string](Get-IemProp $site 'public_host')
    if ($public -and $public -notmatch '^https?://') { $public = 'https://' + $public }
    Invoke-HilUrlCheck -Name 'public-host' -Base $public
    # HIL v2: through the public host the server sees the tunnel (cloudflared on this
    # host, CF-Connecting-IP); on the LAN it sees no tunnel.
    Invoke-HilPeerCheck -Name 'tunnel-peer' -Base $public -Want 'tunnel'
    Invoke-HilPeerCheck -Name 'lan-peer' -Base $lanUrl -Want 'lan'

    # The card over the window: measured 32, callbacks advancing, 0 missed, 0 resets.
    $a = Get-HilStatus
    if ($script:cancelled) { return }
    $b = $a
    $step = [math]::Min(10, [math]::Max(1, $CardSeconds))
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while ($clock.Elapsed.TotalSeconds -lt $CardSeconds) {
        Start-Sleep -Seconds $step
        $b = Get-HilStatus
        if ($script:cancelled) { return }
    }
    $c = Test-IemHilCard -Before (Get-IemProp $a 'engine') -After (Get-IemProp $b 'engine') -Seconds $CardSeconds
    Add-HilCheck 'card' $c.ok $c.detail $c.numbers

    # Server <-> engine over Windows pipes: the DACL holds only the user and SYSTEM.
    $pipesPrivate = Get-IemProp (Get-IemProp $b 'engine') 'pipe_private'
    Add-HilCheck 'pipes' ($pipesPrivate -eq $true) ('engine pipes private: {0}' -f $pipesPrivate)
    # HIL v2: the engine created its pipe's first instance and serves the guard's.
    $po = Test-IemHilPipeOwner -Engine (Get-IemProp $b 'engine')
    Add-HilCheck 'pipe-owner' $po.ok $po.detail $po.numbers

    # The test signal on HIL's spare card outputs (the guard sends it to [guard] hil_tx,
    # outputs no mix uses, so it never reaches a band member; #9, 2026-09-28). The engine's
    # Status carries each spare output's peak since the previous Status (engine.hil): every
    # spare output reaches the asked level in some status read from the signal's start
    # until the first silent one that follows it after the TTL, and that one is silent.
    $r = Invoke-Hil -A @('test-signal', $TestInput, $TestDbfs.ToString($inv), $TestTtl.ToString($inv))
    if ($script:cancelled) { return }
    $sent = Test-IemModeOk -Result $r
    $detail = Get-IemModeText -Result $r
    $during = New-Object System.Collections.ArrayList
    $after = $null
    if ($sent) { $after = Wait-HilSilence -Seen $during -Ttl $TestTtl } else { $after = Get-HilStatus }
    if ($script:cancelled) { return }
    $engine = Get-IemProp $after 'engine'
    $faulted = Get-IemProp $engine 'faulted'
    $sig = Test-IemHilSignal -During $during.ToArray() -After $engine -Dbfs $TestDbfs
    Add-HilCheck 'test-signal' ($sent -and $faulted -eq $false -and $sig.ok) ('{0}; {1}; faulted after the TTL: {2}' -f $detail, $sig.detail, $faulted) $sig.numbers

    # A forced reopen: one more reset, the card back at 32 and streaming.
    $before = Get-HilStatus
    if ($script:cancelled) { return }
    $r = Invoke-Hil -A @('force-reopen')
    if ($script:cancelled) { return }
    $reopened = Test-IemModeOk -Result $r
    # The reset shows once the engine's next Status reached the guard: wait for it.
    $b0 = Get-IemProp $before 'engine'
    if ($reopened) {
        $after = Wait-HilEngine -Callbacks ([int64](Get-IemProp $b0 'callbacks')) -Resets ([int64](Get-IemProp $b0 'resets'))
    } else {
        $after = Get-HilStatus
    }
    if ($script:cancelled) { return }
    $ro = Test-IemHilReopen -Before (Get-IemProp $before 'engine') -After (Get-IemProp $after 'engine')
    Add-HilCheck 'reopen' ($reopened -and $ro.ok) ('{0}; {1}' -f (Get-IemModeText -Result $r), $ro.detail) $ro.numbers
    # HIL v2: that reopen's time (the waited status), within -ReopenMaxMs.
    $rt = Test-IemHilReopenTime -Engine (Get-IemProp $after 'engine') -MaxMs $ReopenMaxMs -Before $b0
    Add-HilCheck 'reopen-time' ($reopened -and $rt.ok) $rt.detail $rt.numbers

    # RT panic -> exit 70, release, respawn, fade-in (design section 7): the
    # guard injects the fault (dev, this job); its engine start count and the
    # last exit code show the respawn (the first status that shows the new
    # engine), and the new engine streams at 32 (a later status with callbacks
    # above that first one's).
    $before = Get-HilStatus
    if ($script:cancelled) { return }
    $r = Invoke-Hil -A @('inject-fault')
    if ($script:cancelled) { return }
    $injected = Test-IemModeOk -Result $r
    $respawned = $null
    $later = $null
    if ($injected) {
        $s0 = [int64](Get-IemProp (Get-IemProp $before 'engine') 'spawns')
        $clock = [Diagnostics.Stopwatch]::StartNew()
        while ($clock.Elapsed.TotalSeconds -lt $PanicWait) {
            Start-Sleep -Milliseconds 500
            $st = Get-HilStatus
            if ($script:cancelled) { return }
            if ([int64](Get-IemProp (Get-IemProp $st 'engine') 'spawns') -gt $s0) { $respawned = $st; break }
        }
        if ($null -ne $respawned) {
            $later = Wait-HilEngine -Callbacks ([int64](Get-IemProp (Get-IemProp $respawned 'engine') 'callbacks'))
            if ($script:cancelled) { return }
        }
    }
    $pk = Test-IemHilPanic -Before (Get-IemProp $before 'engine') -Respawned (Get-IemProp $respawned 'engine') -Later (Get-IemProp $later 'engine')
    Add-HilCheck 'panic' ($injected -and $pk.ok) ('{0}; {1}' -f (Get-IemModeText -Result $r), $pk.detail) $pk.numbers
    # HIL v2: the faulting callback's time, which the guard keeps across the respawn;
    # changed from the value before the injection (it is not tied to one engine start).
    $ft = Test-IemHilFaultTime -Before (Get-IemProp $before 'engine') -After (Get-IemProp $later 'engine') -MaxUs $FaultMaxUs
    Add-HilCheck 'fault-time' ($injected -and $ft.ok) $ft.detail $ft.numbers

    # The alarm push: iemmode alarm-test must reach at least one of the
    # engineer's devices (the PWA's notification subscriptions). HIL v2 reads the
    # highest alarm id first, so its own test alarm is the one new above it.
    $pre = Get-HilStatus
    if ($script:cancelled) { return }
    $top = Get-IemHilMaxAlarmId -Reply $pre
    $r = Invoke-Hil -A @('alarm-test')
    if ($script:cancelled) { return }
    $pushed = Test-IemModeOk -Result $r
    Add-HilCheck 'alarm-push' $pushed (Get-IemModeText -Result $r)
    if ($pushed) {
        Invoke-HilAlarmAck -Reply $r.reply -Above $top
        if ($script:cancelled) { return }
    }

    # F30: a synthetic site change and its revert (the three files together, Test-IemHilV2Inputs).
    if ($SiteChange) { Invoke-HilF30 }
}

function Invoke-HilAlarmAck {
    # HIL v2: acknowledge this run's test alarm, then earlier runs' (HIL v1 never did):
    # only the guard's exact test text with no step and no owner question, never another
    # alarm, and nothing at all when this run's own is not exactly one above -Above.
    param($Reply, $Above)
    if ($null -eq $Above) { Add-HilCheck 'alarm-ack' $false 'no alarm list read before alarm-test: nothing acknowledged'; return }
    $ta = Get-IemHilTestAlarms -Reply $Reply -Above ([int64]$Above)
    if ($null -eq $ta.own) { Add-HilCheck 'alarm-ack' $false ($ta.detail + ': nothing acknowledged'); return }
    $failed = @()
    foreach ($id in @(@($ta.own) + @($ta.stale))) {
        $a = Invoke-Hil -A @('alarm-ack', ([string]$id))
        if ($script:cancelled) { return }
        if (-not (Test-IemModeOk -Result $a)) { $failed += ('{0}: {1}' -f $id, (Get-IemModeText -Result $a)) }
    }
    $detail = '{0}; acknowledged' -f $ta.detail
    if ($failed.Count -gt 0) { $detail = '{0}; not acknowledged: {1}' -f $ta.detail, ($failed -join '; ') }
    Add-HilCheck 'alarm-ack' ($failed.Count -eq 0) $detail ([pscustomobject]@{ own = $ta.own; stale = @($ta.stale) })
}

function Invoke-HilF30 {
    # The synthetic site change and its revert through install-site; the installed site's
    # sha256 before the change, after it and after the revert (Test-IemHilSiteRestored).
    # Nothing is installed unless all three files read and the revert file is the installed
    # site byte for byte (it can restore it) and the change file is not (Test-IemHilF30Ready).
    $h0 = ''
    $hc = ''
    $hr = ''
    try {
        $h0 = Get-IemHilFileSha256 -Path $SiteInstalled
        $hc = Get-IemHilFileSha256 -Path $SiteChange
        $hr = Get-IemHilFileSha256 -Path $SiteRevert
    } catch {
        Add-HilCheck 'f30' $false ('F30 files: {0}; nothing installed' -f $_.Exception.Message); return
    }
    $ready = Test-IemHilF30Ready -Installed $h0 -Change $hc -Revert $hr
    if (-not $ready.ok) { Add-HilCheck 'f30' $false ('{0}; nothing installed' -f $ready.detail); return }
    $r1 = Invoke-Hil -A @('install-site', $SiteChange)
    if ($script:cancelled) { return }
    $h1 = ''
    try { $h1 = Get-IemHilFileSha256 -Path $SiteInstalled } catch { $h1 = '' }
    $r2 = Invoke-Hil -A @('install-site', $SiteRevert)
    if ($script:cancelled) {
        # The guard left dev at the revert: the change may still be installed (private `why`).
        if (Test-IemModeOk -Result $r1) { $script:why += '; f30: the change was installed and its revert did not run: the installed site may still hold the change' }
        return
    }
    $h2 = ''
    try { $h2 = Get-IemHilFileSha256 -Path $SiteInstalled } catch { $h2 = '' }
    $sr = Test-IemHilSiteRestored -Before $h0 -Changed $h1 -After $h2
    Add-HilCheck 'f30' ((Test-IemModeOk -Result $r1) -and (Test-IemModeOk -Result $r2) -and $sr.ok) `
        ('change: {0}; revert: {1}; {2}' -f (Get-IemModeText -Result $r1), (Get-IemModeText -Result $r2), $sr.detail) $sr.numbers
}

$started = (Get-Date).ToUniversalTime().ToString('o')
$begun = $false
$problems = Test-IemHilInputs -Sha $Sha -Branch $Branch -JobRun $JobRun -Out $Out -ScriptDir $PSScriptRoot -TestDbfs $TestDbfs -TestTtl $TestTtl
$v2Problems = Test-IemHilV2Inputs -ReopenMaxMs $ReopenMaxMs -FaultMaxUs $FaultMaxUs -SiteChange $SiteChange -SiteRevert $SiteRevert `
    -SiteInstalled $SiteInstalled
$problems = @($problems) + @($v2Problems)
if ($Out) {
    Write-IemJsonFile -Path $Out -Value (New-IemHilResult -Conclusion 'failure' -Summary 'HIL v1 did not finish' -Sha $Sha -Branch $Branch -JobRun $JobRun -Started $started)
}
if ($problems.Count -gt 0) {
    Add-HilCheck 'inputs' $false ($problems -join '; ')
} else {
    try {
        $r = Invoke-IemMode -Exe $Iemmode -Arguments @('job-begin', $JobRun)
        if (Test-IemModeOk -Result $r) {
            $begun = $true
            Invoke-HilChecks
        } elseif ($null -ne $r.reply) {
            $script:cancelled = $true
            $script:reason = 'not-free'
            $script:why = 'job-begin refused: ' + (Get-IemModeText -Result $r)
        } else {
            Add-HilCheck 'job-begin' $false (Get-IemModeText -Result $r)
        }
    } catch {
        Add-HilCheck 'script' $false ('hil-v1.ps1: ' + $_.Exception.Message)
    }
}
if ($begun) {
    $r = Invoke-IemMode -Exe $Iemmode -Arguments @('job-end', $JobRun)
    if (-not $script:cancelled -and -not (Test-IemModeOk -Result $r)) { Add-HilCheck 'job-end' $false (Get-IemModeText -Result $r) }
}
$conclusion = Get-IemHilConclusion -Checks $script:checks.ToArray() -Cancelled:$script:cancelled
$summary = Get-IemHilSummary -Conclusion $conclusion -Checks $script:checks.ToArray() -Reason $script:reason
if ($begun -and $conclusion -ne 'cancelled') {
    $hil = 'red'
    if ($conclusion -eq 'success') { $hil = 'green' }
    $r = Invoke-IemMode -Exe $Iemmode -Arguments @('report', $Sha, $hil, $summary)
    if (-not (Test-IemModeOk -Result $r)) {
        Add-HilCheck 'report' $false (Get-IemModeText -Result $r)
        $conclusion = Get-IemHilConclusion -Checks $script:checks.ToArray()
        $summary = Get-IemHilSummary -Conclusion $conclusion -Checks $script:checks.ToArray()
    }
}
$result = New-IemHilResult -Conclusion $conclusion -Summary $summary -Sha $Sha -Branch $Branch -JobRun $JobRun -Started $started `
    -Checks $script:checks.ToArray() -Why $script:why
if ($Out) { Write-IemJsonFile -Path $Out -Value $result }
Write-Output (ConvertTo-Json -InputObject $result -Depth 8)
if ($conclusion -eq 'failure') { exit 1 }
exit 0
