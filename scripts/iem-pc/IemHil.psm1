#Requires -Version 5.1
# HIL v2's checks (S7 design section 7, plan Task 31; #10), pure: hil-v1.ps1
# imports this module beside IemPc.psm1 (it rides in the bundle next to the
# script, not required, so older bundles still install) and judges with it:
# - pipe-owner: the engine serves its control pipe (the guard's supervisor
#   connection's server process is the engine's pid) and its DACL is private;
# - tunnel-peer and lan-peer: /api/peer's fixed codes through the public host
#   and on the LAN;
# - reopen-time: the forced reopen's time within -ReopenMaxMs;
# - fault-time: the faulting callback's time under -FaultMaxUs, changed by
#   this injection (the guard keeps it across respawns, not per engine start);
# - alarm-ack: which alarms are the guard's own test alarm (the one new above
#   the id read before alarm-test) and the stale ones of earlier runs: only the
#   exact test text, no step, no owner question, not yet acknowledged;
# - f30: nothing is installed unless the revert file is the installed site byte for
#   byte and the change file is not; the installed site's sha256 after the revert
#   is the one before.
# Every field an older guard or engine does not send fails its check with
# "lacks '<field>'" (an older engine's field reads 0 or null in a newer
# guard: the same). Nothing is ended by force (I8); no site value (P6).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
# Get-IemProp (reads a parsed JSON object under strict mode); a no-op when loaded.
Import-Module (Join-Path $PSScriptRoot 'IemPc.psm1') -Global

# The guard's test alarm (crates/iem-guard/src/daemon.rs, alarm_test): alarm-ack
# acknowledges no other text.
$script:HilTestAlarmText = 'alarm test (iemmode alarm-test)'
# /api/peer's codes (crates/iem-server/src/peer_route.rs).
$script:PeerOrigins = @('tunnel', 'lan')
$script:PeerKinds = @('loopback', 'host', 'other')
$script:NoEngine = 'iemmode status carries no engine status'

function New-IemHilVerdict {
    param([Parameter(Mandatory)][bool]$Ok, [string]$Detail = '', $Numbers = $null)
    [pscustomobject]@{ ok = $Ok; detail = $Detail; numbers = $Numbers }
}

function Get-IemHilLack {
    # '' when $Object carries every field with a value, else "<what> lacks '<field>'"
    # (missing; " (null)" when null; " (0)" when -NonZero names it and it is 0).
    param($Object, [Parameter(Mandatory)][string[]]$Fields, [string[]]$NonZero = @(), [string]$What = 'the engine status')
    foreach ($f in $Fields) {
        $p = $Object.PSObject.Properties[$f]
        if ($null -eq $p) { return ("{0} lacks '{1}'" -f $What, $f) }
        if ($null -eq $p.Value) { return ("{0} lacks '{1}' (null)" -f $What, $f) }
        if (($NonZero -ccontains $f) -and ([double]$p.Value -eq 0)) { return ("{0} lacks '{1}' (0)" -f $What, $f) }
    }
    return ''
}

function Get-IemHilTestAlarmText {
    return $script:HilTestAlarmText
}

function Test-IemHilPipeOwner {
    # The engine created its control pipe's first instance and serves the guard's
    # supervisor connection: pipe_server_pid (GetNamedPipeServerProcessId on that
    # connection) is the engine's pid; and the pipes' DACL holds only the user and SYSTEM.
    param($Engine)
    if ($null -eq $Engine) { return (New-IemHilVerdict $false $script:NoEngine) }
    $lack = Get-IemHilLack -Object $Engine -Fields @('pid', 'pipe_server_pid', 'pipe_private')
    if ($lack) { return (New-IemHilVerdict $false $lack) }
    $enginePid = [int64]$Engine.pid
    $server = [int64]$Engine.pipe_server_pid
    $numbers = [pscustomobject]@{ pid = $enginePid; pipe_server_pid = $server }
    $problems = @()
    if ($server -ne $enginePid) { $problems += ('the pipe is served by process {0}, the engine is {1}' -f $server, $enginePid) }
    if ($Engine.pipe_private -ne $true) { $problems += 'the engine pipes are not private' }
    if ($problems.Count -gt 0) { return (New-IemHilVerdict $false ($problems -join '; ') $numbers) }
    return (New-IemHilVerdict $true ('the engine (pid {0}) serves its control pipe; pipes private' -f $enginePid) $numbers)
}

function Test-IemHilPeer {
    # /api/peer's answer (origin tunnel|lan, peer loopback|host|other: fixed codes, no
    # address). -Want tunnel: through the public host the server saw the tunnel, from
    # this host (loopback or one of its addresses). -Want lan: on the LAN it saw no tunnel.
    param($Peer, [Parameter(Mandatory)][ValidateSet('tunnel', 'lan')][string]$Want)
    if ($null -eq $Peer) { return (New-IemHilVerdict $false 'no answer from /api/peer') }
    $lack = Get-IemHilLack -Object $Peer -Fields @('origin', 'peer') -What 'the answer'
    if ($lack) { return (New-IemHilVerdict $false $lack) }
    $origin = [string]$Peer.origin
    $kind = [string]$Peer.peer
    if (($script:PeerOrigins -cnotcontains $origin) -or ($script:PeerKinds -cnotcontains $kind)) {
        return (New-IemHilVerdict $false 'the answer holds a code /api/peer does not have')
    }
    $numbers = [pscustomobject]@{ origin = $origin; peer = $kind }
    $ok = $origin -ceq $Want
    if ($Want -ceq 'tunnel') { $ok = $ok -and ($kind -ceq 'loopback' -or $kind -ceq 'host') }
    $detail = 'origin {0}, peer {1}' -f $origin, $kind
    if (-not $ok) {
        $wanted = 'lan'
        if ($Want -ceq 'tunnel') { $wanted = 'tunnel from loopback or host' }
        $detail = '{0} (want {1})' -f $detail, $wanted
    }
    return (New-IemHilVerdict $ok $detail $numbers)
}

function Test-IemHilReopenTime {
    # The forced reopen's time (Status.last_reopen_us: the old stream's stop to the new
    # one's measured period) within -MaxMs. 0 is no reopen time (an older engine). With
    # -Before (the status before the reopen) a time it already held is an earlier reopen's.
    param($Engine, [Parameter(Mandatory)][double]$MaxMs, $Before = $null)
    if ($null -eq $Engine) { return (New-IemHilVerdict $false $script:NoEngine) }
    $lack = Get-IemHilLack -Object $Engine -Fields @('last_reopen_us') -NonZero @('last_reopen_us')
    if ($lack) { return (New-IemHilVerdict $false $lack) }
    $us = [int64]$Engine.last_reopen_us
    $ms = $us / 1000.0
    $inv = [Globalization.CultureInfo]::InvariantCulture
    $numbers = [pscustomobject]@{ reopen_ms = $ms; max_ms = $MaxMs }
    $was = Get-IemProp $Before 'last_reopen_us'
    if ($null -ne $was -and [int64]$was -ne 0 -and [int64]$was -eq $us) {
        return (New-IemHilVerdict $false ([string]::Format($inv, 'last_reopen_us unchanged by the reopen ({0} us, an earlier reopen)', $us)) $numbers)
    }
    $ok = ($us -gt 0) -and ($ms -le $MaxMs)
    $detail = [string]::Format($inv, 'reopen {0:0.0} ms (bound {1} ms)', $ms, $MaxMs)
    return (New-IemHilVerdict $ok $detail $numbers)
}

function Test-IemHilFaultTime {
    # The faulting callback's own time (Reply.engine.last_fault_us, which the guard keeps
    # across the respawn) under -MaxUs, and changed from -Before (the status read before
    # the injection): the value is not bound to one engine start, so an injection that
    # never faulted would leave an earlier fault's time.
    param($Before, $After, [Parameter(Mandatory)][double]$MaxUs)
    if ($null -eq $After) { return (New-IemHilVerdict $false $script:NoEngine) }
    $lack = Get-IemHilLack -Object $After -Fields @('last_fault_us') -NonZero @('last_fault_us')
    if ($lack) { return (New-IemHilVerdict $false $lack) }
    if ($null -eq $Before) { return (New-IemHilVerdict $false 'no engine status before the injection') }
    $us = [double]$After.last_fault_us
    $inv = [Globalization.CultureInfo]::InvariantCulture
    $numbers = [pscustomobject]@{ fault_us = $us; max_us = $MaxUs }
    $was = Get-IemProp $Before 'last_fault_us'
    if ($null -ne $was -and [double]$was -eq $us) {
        return (New-IemHilVerdict $false ([string]::Format($inv, 'last_fault_us unchanged by the injection ({0} us, an earlier fault)', $us)) $numbers)
    }
    $ok = (-not [double]::IsNaN($us)) -and (-not [double]::IsInfinity($us)) -and ($us -gt 0) -and ($us -lt $MaxUs)
    $detail = [string]::Format($inv, 'faulting callback {0:0.0} us (under {1} us)', $us, $MaxUs)
    return (New-IemHilVerdict $ok $detail $numbers)
}

function Get-IemHilMaxAlarmId {
    # The highest alarm id in a guard reply (0 without alarms); $null without a reply or
    # without its 'alarms' (an id HIL could not read: it acknowledges nothing then).
    param($Reply)
    if ($null -eq $Reply -or $null -eq $Reply.PSObject.Properties['alarms']) { return $null }
    $top = [int64]0
    foreach ($a in @(Get-IemProp $Reply 'alarms')) {
        $i = Get-IemProp $a 'id'
        if ($null -ne $i -and [int64]$i -gt $top) { $top = [int64]$i }
    }
    return $top
}

function Test-IemHilIsTestAlarm {
    # The guard's test alarm as alarm_test raises it and nobody acknowledged it: the exact
    # text, the step null, no owner question, not acknowledged. A field missing (an alarm
    # of another shape) is never one.
    param($Alarm)
    if ($null -eq $Alarm) { return $false }
    foreach ($f in @('id', 'text', 'step', 'acked', 'owner_question')) { if ($null -eq $Alarm.PSObject.Properties[$f]) { return $false } }
    if ($null -eq $Alarm.id -or [int64]$Alarm.id -lt 1) { return $false }
    if (([string]$Alarm.text) -cne $script:HilTestAlarmText) { return $false }
    if ($null -ne $Alarm.step) { return $false }
    if (-not ($Alarm.acked -is [bool]) -or $Alarm.acked) { return $false }
    if (-not ($Alarm.owner_question -is [bool]) -or $Alarm.owner_question) { return $false }
    return $true
}

function Get-IemHilTestAlarms {
    # The test alarms of a reply (alarm-test's own): `own` is the one id above -Above (the
    # highest id read before alarm-test), $null unless exactly one; `stale` the ids at or
    # below it, earlier runs' (HIL v1 never acknowledged its own), ascending; `detail`.
    param($Reply, [Parameter(Mandatory)][int64]$Above)
    if ($null -eq $Reply -or $null -eq $Reply.PSObject.Properties['alarms']) {
        return [pscustomobject]@{ own = $null; stale = @(); detail = "the reply lacks 'alarms'" }
    }
    $mine = @()
    $stale = @()
    foreach ($a in @(Get-IemProp $Reply 'alarms')) {
        if (-not (Test-IemHilIsTestAlarm -Alarm $a)) { continue }
        if ([int64]$a.id -gt $Above) { $mine += [int64]$a.id } else { $stale += [int64]$a.id }
    }
    $stale = @($stale | Sort-Object)
    if ($mine.Count -eq 1) {
        return [pscustomobject]@{ own = $mine[0]; stale = $stale; detail = ('own test alarm {0}, stale {1}' -f $mine[0], $stale.Count) }
    }
    $detail = 'no test alarm above {0}' -f $Above
    if ($mine.Count -gt 1) { $detail = '{0} test alarms above {1}: which one is this run''s is unknown' -f $mine.Count, $Above }
    return [pscustomobject]@{ own = $null; stale = @(); detail = $detail }
}

function Test-IemHilSiteRestored {
    # F30 by bytes: the installed site's sha256 after the revert equals the one before
    # the change, and the change reached the installed file (else -SiteInstalled is not
    # the file install-site writes and the equality proves nothing). '' = unread.
    param([string]$Before = '', [string]$Changed = '', [string]$After = '')
    $hex = '^[0-9a-f]{64}$'
    $numbers = [pscustomobject]@{ before = $Before; changed = $Changed; after = $After; restored = $false }
    if ($Before -cnotmatch $hex) { return (New-IemHilVerdict $false 'the installed site was not read before the change' $numbers) }
    if ($Changed -cnotmatch $hex) { return (New-IemHilVerdict $false 'the installed site was not read after the change' $numbers) }
    if ($After -cnotmatch $hex) { return (New-IemHilVerdict $false 'the installed site was not read after the revert' $numbers) }
    if ($Changed -ceq $Before) { return (New-IemHilVerdict $false 'the change never reached the installed site' $numbers) }
    if ($After -cne $Before) { return (New-IemHilVerdict $false 'the installed site after the revert differs from before the change' $numbers) }
    $numbers.restored = $true
    return (New-IemHilVerdict $true 'the installed site is byte for byte the one before the change' $numbers)
}

function Test-IemHilF30Ready {
    # Before any install-site (the lane's review): the revert file is the installed site
    # byte for byte, so the revert can restore it, and the change file is not, so the change
    # changes something. Else nothing is installed: a revert of other bytes would leave the
    # PC on a site that is not the original. '' = unread.
    param([string]$Installed = '', [string]$Change = '', [string]$Revert = '')
    $hex = '^[0-9a-f]{64}$'
    if ($Installed -cnotmatch $hex) { return (New-IemHilVerdict $false 'the installed site could not be read') }
    if ($Change -cnotmatch $hex) { return (New-IemHilVerdict $false 'the change file could not be read') }
    if ($Revert -cnotmatch $hex) { return (New-IemHilVerdict $false 'the revert file could not be read') }
    if ($Revert -cne $Installed) { return (New-IemHilVerdict $false 'the revert file is not the installed site byte for byte: it could not restore it') }
    if ($Change -ceq $Installed) { return (New-IemHilVerdict $false 'the change file is the installed site: the change would change nothing') }
    return (New-IemHilVerdict $true 'the revert restores the installed site, the change changes it')
}

function Get-IemHilFileSha256 {
    # A file's sha256, lowercase hex.
    param([Parameter(Mandatory)][string]$Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Test-IemHilV2Inputs {
    # HIL v2's own inputs (hil.yml passes F30's three files, Task 32). Returns the problems.
    param([double]$ReopenMaxMs = 150, [double]$FaultMaxUs = 1000, [string]$SiteChange = '', [string]$SiteRevert = '',
          [string]$SiteInstalled = '')
    $p = @()
    if ([double]::IsNaN($ReopenMaxMs) -or [double]::IsInfinity($ReopenMaxMs) -or $ReopenMaxMs -le 0) { $p += 'ReopenMaxMs: a bound of more than 0 ms' }
    if ([double]::IsNaN($FaultMaxUs) -or [double]::IsInfinity($FaultMaxUs) -or $FaultMaxUs -le 0) { $p += 'FaultMaxUs: a bound of more than 0 us' }
    $given = @(@($SiteChange, $SiteRevert, $SiteInstalled) | Where-Object { $_ }).Count
    if ($given -ne 0 -and $given -ne 3) { $p += 'F30: -SiteChange, -SiteRevert and -SiteInstalled together' }
    return ,$p
}

Export-ModuleMember -Function Get-IemHilTestAlarmText, Test-IemHilPipeOwner, Test-IemHilPeer, Test-IemHilReopenTime,
    Test-IemHilFaultTime, Get-IemHilMaxAlarmId, Test-IemHilIsTestAlarm, Get-IemHilTestAlarms, Test-IemHilSiteRestored, Test-IemHilF30Ready,
    Get-IemHilFileSha256, Test-IemHilV2Inputs
