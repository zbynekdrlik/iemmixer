#Requires -Version 5.1
# Self-test of hil-v1.ps1 (HIL v1, #9; v2, #10) on Windows PowerShell 5.1 (CI
# job windows, an ephemeral administrator runner): IemHil.psm1's HIL v2 checks
# (pure), then the script end to end against a stand-in for iemmode, from a
# temp bundles\<sha>\ folder. No site value (P6): synthetic pids, alarms and
# site text only.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
Import-Module (Join-Path $here 'IemPc.psm1') -Force
Import-Module (Join-Path $here 'IemHil.psm1') -Force
function Assert($cond, $what) { if (-not $cond) { throw "FAILED: $what" } ; Write-Host "ok  $what" }

$id = [guid]::NewGuid().ToString('N').Substring(0, 8)
$base = Join-Path ([IO.Path]::GetTempPath()) ('iem-hil-test-' + $id)
$S = '0123456789abcdef0123456789abcdef01234567'
New-Item -ItemType Directory -Force -Path $base | Out-Null

try {
    # ---- HIL v2's checks (IemHil.psm1, pure; plan Task 31) ----
    # pipe-owner: the engine serves the guard's supervisor connection (its pid is the
    # pipe's server, GetNamedPipeServerProcessId) and its pipes' DACL is private.
    function EngO($enginePid, $server, $private = $true) { [pscustomobject]@{ pid = $enginePid; pipe_server_pid = $server; pipe_private = $private } }
    $po = Test-IemHilPipeOwner -Engine (EngO 4242 4242)
    Assert ($po.ok -and $po.numbers.pid -eq 4242 -and $po.numbers.pipe_server_pid -eq 4242) "hil-pipe-owner-the-engine-serves-its-pipe ($($po.detail))"
    $po = Test-IemHilPipeOwner -Engine (EngO 4242 4243)
    Assert (-not $po.ok -and $po.detail -like '*4243*4242*') "hil-pipe-owner-refuses-another-server ($($po.detail))"
    $po = Test-IemHilPipeOwner -Engine (EngO $null 4242)
    Assert (-not $po.ok -and $po.detail -like "*lacks 'pid'*") "hil-pipe-owner-refuses-no-engine-pid ($($po.detail))"
    $po = Test-IemHilPipeOwner -Engine (EngO 4242 $null)
    Assert (-not $po.ok -and $po.detail -like "*lacks 'pipe_server_pid'*") "hil-pipe-owner-refuses-an-unread-server ($($po.detail))"
    Assert (-not (Test-IemHilPipeOwner -Engine (EngO 4242 4242 $false)).ok) 'hil-pipe-owner-refuses-a-dacl-not-private'
    $po = Test-IemHilPipeOwner -Engine ([pscustomobject]@{ pid = 4242; pipe_private = $true })
    Assert (-not $po.ok -and $po.detail -ceq "the engine status lacks 'pipe_server_pid'") "hil-pipe-owner-refuses-an-older-guard ($($po.detail))"
    Assert ((Test-IemHilPipeOwner -Engine $null).detail -ceq 'iemmode status carries no engine status') 'hil-pipe-owner-refuses-no-engine-status'

    # tunnel-peer and lan-peer: /api/peer's fixed codes (origin tunnel|lan, peer loopback|host|other).
    function Peer([string]$o, [string]$p) { [pscustomobject]@{ origin = $o; peer = $p } }
    foreach ($p in @('loopback', 'host')) {
        $pr = Test-IemHilPeer -Peer (Peer 'tunnel' $p) -Want 'tunnel'
        Assert ($pr.ok -and $pr.numbers.origin -ceq 'tunnel' -and $pr.numbers.peer -ceq $p) "hil-peer-tunnel-from-loopback-or-host [$p] ($($pr.detail))"
    }
    foreach ($a in @((Peer 'lan' 'loopback'), (Peer 'lan' 'host'), (Peer 'lan' 'other'), (Peer 'tunnel' 'other'))) {
        Assert (-not (Test-IemHilPeer -Peer $a -Want 'tunnel').ok) "hil-peer-refuses-lan-or-other-through-the-public-host [$($a.origin) $($a.peer)]"
    }
    foreach ($p in @('loopback', 'host', 'other')) { Assert (Test-IemHilPeer -Peer (Peer 'lan' $p) -Want 'lan').ok "hil-lan-peer-is-lan [$p]" }
    Assert (-not (Test-IemHilPeer -Peer (Peer 'tunnel' 'loopback') -Want 'lan').ok) 'hil-lan-peer-refuses-the-tunnel'
    foreach ($a in @((Peer 'Tunnel' 'loopback'), (Peer 'tunnel' 'LOOPBACK'), (Peer 'tunnel' '10.0.0.10'), (Peer '' 'host'))) {
        Assert (-not (Test-IemHilPeer -Peer $a -Want 'tunnel').ok) "hil-peer-refuses-a-code-it-does-not-know [$($a.origin) $($a.peer)]"
    }
    $pr = Test-IemHilPeer -Peer ([pscustomobject]@{ peer = 'loopback' }) -Want 'tunnel'
    Assert (-not $pr.ok -and $pr.detail -ceq "the answer lacks 'origin'") "hil-peer-refuses-an-answer-without-origin ($($pr.detail))"
    Assert (-not (Test-IemHilPeer -Peer $null -Want 'lan').ok) 'hil-peer-refuses-no-answer'

    # reopen-time: the forced reopen's time within the bound (S1a: about 104 ms at 64).
    function EngR($us) { [pscustomobject]@{ last_reopen_us = $us } }
    $rt = Test-IemHilReopenTime -Engine (EngR 104000) -MaxMs 200
    Assert ($rt.ok -and $rt.numbers.reopen_ms -eq 104 -and $rt.numbers.max_ms -eq 200) "hil-reopen-time-within-the-bound ($($rt.detail))"
    Assert (Test-IemHilReopenTime -Engine (EngR 200000) -MaxMs 200).ok 'hil-reopen-time-at-the-bound'
    Assert (-not (Test-IemHilReopenTime -Engine (EngR 201000) -MaxMs 200).ok) 'hil-reopen-time-refuses-201-ms'
    $rt = Test-IemHilReopenTime -Engine (EngR 0) -MaxMs 200
    Assert (-not $rt.ok -and $rt.detail -like "*lacks 'last_reopen_us'*") "hil-reopen-time-refuses-0 ($($rt.detail))"
    $rt = Test-IemHilReopenTime -Engine ([pscustomobject]@{ frames = 32 }) -MaxMs 200
    Assert (-not $rt.ok -and $rt.detail -ceq "the engine status lacks 'last_reopen_us'") "hil-reopen-time-refuses-a-missing-field ($($rt.detail))"
    Assert ((Test-IemHilReopenTime -Engine (EngR 104000) -MaxMs 200 -Before (EngR 0)).ok) 'hil-reopen-time-after-none-before'
    $rt = Test-IemHilReopenTime -Engine (EngR 104000) -MaxMs 200 -Before (EngR 104000)
    Assert (-not $rt.ok -and $rt.detail -like '*unchanged*') "hil-reopen-time-refuses-an-earlier-reopens-time ($($rt.detail))"
    Assert ((Test-IemHilReopenTime -Engine $null -MaxMs 200).detail -ceq 'iemmode status carries no engine status') 'hil-reopen-time-refuses-no-engine-status'

    # fault-time: the faulting callback's time under 1 ms, changed by this injection
    # (last_fault_us is the guard's, kept across respawns, not tied to one engine start).
    function EngF($us) { [pscustomobject]@{ last_fault_us = $us } }
    $ft = Test-IemHilFaultTime -Before (EngF $null) -After (EngF 412.5) -MaxUs 1000
    Assert ($ft.ok -and $ft.numbers.fault_us -eq 412.5 -and $ft.numbers.max_us -eq 1000) "hil-fault-time-under-1-ms ($($ft.detail))"
    Assert ((Test-IemHilFaultTime -Before (EngF 300) -After (EngF 999.9) -MaxUs 1000).ok) 'hil-fault-time-a-newer-fault-just-under-1-ms'
    foreach ($us in @(0, 1000, 5000, -1)) {
        Assert (-not (Test-IemHilFaultTime -Before (EngF $null) -After (EngF $us) -MaxUs 1000).ok) "hil-fault-time-refuses [$us]"
    }
    $ft = Test-IemHilFaultTime -Before (EngF $null) -After (EngF $null) -MaxUs 1000
    Assert (-not $ft.ok -and $ft.detail -like "*lacks 'last_fault_us'*") "hil-fault-time-refuses-null ($($ft.detail))"
    $ft = Test-IemHilFaultTime -Before (EngF $null) -After ([pscustomobject]@{ frames = 32 }) -MaxUs 1000
    Assert (-not $ft.ok -and $ft.detail -ceq "the engine status lacks 'last_fault_us'") "hil-fault-time-refuses-a-missing-field ($($ft.detail))"
    $ft = Test-IemHilFaultTime -Before (EngF 412.5) -After (EngF 412.5) -MaxUs 1000
    Assert (-not $ft.ok -and $ft.detail -like '*unchanged*') "hil-fault-time-refuses-an-earlier-faults-time ($($ft.detail))"
    $ft = Test-IemHilFaultTime -Before $null -After (EngF 412.5) -MaxUs 1000
    Assert (-not $ft.ok -and $ft.detail -like '*before the injection*') "hil-fault-time-refuses-no-status-before-the-injection ($($ft.detail))"
    Assert ((Test-IemHilFaultTime -Before (EngF $null) -After $null -MaxUs 1000).detail -ceq 'iemmode status carries no engine status') 'hil-fault-time-refuses-no-engine-status'

    # alarm-ack: only the guard's exact test alarm text, with no step, no owner question,
    # not yet acknowledged; its own is the one new above the id read before alarm-test.
    $T = Get-IemHilTestAlarmText
    $repo = Split-Path -Parent (Split-Path -Parent $here)
    $daemon = [IO.File]::ReadAllText([IO.Path]::Combine($repo, 'crates', 'iem-guard', 'src', 'daemon.rs'))
    Assert ($T -ceq 'alarm test (iemmode alarm-test)' -and $daemon.Contains('"' + $T + '"')) 'hil-test-alarm-text-is-the-guards'
    function Al($i, $text, $step = $null, $acked = $false, $q = $false) {
        [pscustomobject]@{ id = $i; at = 1; step = $step; text = $text; acked = $acked; notified = $true; owner_question = $q }
    }
    $alarms = @((Al 1 $T $null $true), (Al 2 'EngineStop: the engine did not stop' 'engine_stop' $false $true), (Al 3 $T),
                (Al 4 $T $null $false $true), (Al 5 $T 'engine_stop'), (Al 6 ($T + ' ')), (Al 7 $T.ToUpperInvariant()),
                ([pscustomobject]@{ id = 8; at = 1; text = $T; acked = $false; owner_question = $false }), (Al 9 $T))
    $reply = [pscustomobject]@{ ok = $true; alarms = $alarms }
    Assert ((Get-IemHilMaxAlarmId -Reply $reply) -eq 9) 'hil-max-alarm-id'
    Assert ((Get-IemHilMaxAlarmId -Reply ([pscustomobject]@{ alarms = @() })) -eq 0) 'hil-max-alarm-id-without-alarms'
    Assert ($null -eq (Get-IemHilMaxAlarmId -Reply ([pscustomobject]@{ ok = $true }))) 'hil-max-alarm-id-of-a-reply-without-alarms-is-unknown'
    Assert ($null -eq (Get-IemHilMaxAlarmId -Reply $null)) 'hil-max-alarm-id-without-a-reply-is-unknown'
    $ta = Get-IemHilTestAlarms -Reply $reply -Above 8
    Assert ($ta.own -eq 9) "hil-own-test-alarm-is-the-new-one ($($ta.detail))"
    Assert ((@($ta.stale) -join ',') -ceq '3') "hil-stale-test-alarms-are-only-the-exact-text-unacked-without-step-or-question ($(@($ta.stale) -join ','))"
    $ta = Get-IemHilTestAlarms -Reply $reply -Above 9
    Assert ($null -eq $ta.own -and $ta.detail -like '*no test alarm above 9*') "hil-own-test-alarm-refuses-none-above ($($ta.detail))"
    $ta = Get-IemHilTestAlarms -Reply ([pscustomobject]@{ alarms = @((Al 3 $T), (Al 10 $T), (Al 11 $T)) }) -Above 9
    Assert ($null -eq $ta.own -and $ta.detail -like '*2 test alarms above 9*' -and @($ta.stale).Count -eq 0) "hil-own-test-alarm-refuses-two-above ($($ta.detail))"
    $ta = Get-IemHilTestAlarms -Reply ([pscustomobject]@{ alarms = @((Al 10 $T 'engine_stop')) }) -Above 9
    Assert ($null -eq $ta.own) 'hil-own-test-alarm-refuses-one-with-a-step'
    $ta = Get-IemHilTestAlarms -Reply ([pscustomobject]@{ ok = $true }) -Above 0
    Assert ($null -eq $ta.own -and $ta.detail -like "*lacks 'alarms'*") "hil-test-alarms-refuse-a-reply-without-alarms ($($ta.detail))"

    # F30: the installed site's sha256 after the revert is the one before the change, and
    # the change reached it (else the path is not the installed site).
    $h0 = 'a' * 64
    $hc = 'b' * 64
    $sr = Test-IemHilSiteRestored -Before $h0 -Changed $hc -After $h0
    Assert ($sr.ok -and $sr.numbers.restored) "hil-site-restored-by-bytes ($($sr.detail))"
    $sr = Test-IemHilSiteRestored -Before $h0 -Changed $hc -After $hc
    Assert (-not $sr.ok -and $sr.detail -like '*differs*') "hil-site-refuses-a-revert-that-differs ($($sr.detail))"
    $sr = Test-IemHilSiteRestored -Before $h0 -Changed $h0 -After $h0
    Assert (-not $sr.ok -and $sr.detail -like '*change*') "hil-site-refuses-a-change-that-never-reached-it ($($sr.detail))"
    foreach ($bad in @(@('', $hc, $h0), @($h0, $hc, ''), @($h0, '', $h0))) {
        Assert (-not (Test-IemHilSiteRestored -Before $bad[0] -Changed $bad[1] -After $bad[2]).ok) "hil-site-refuses-an-unread-hash [$($bad[0].Length) $($bad[1].Length) $($bad[2].Length)]"
    }

    # HIL v2's inputs: the bounds, and F30's three files together.
    Assert ((Test-IemHilV2Inputs -ReopenMaxMs 200 -FaultMaxUs 1000).Count -eq 0) 'hil-v2-inputs-the-defaults'
    Assert ((Test-IemHilV2Inputs -ReopenMaxMs 200 -FaultMaxUs 1000 -SiteChange 'c' -SiteRevert 'r' -SiteInstalled 'i').Count -eq 0) 'hil-v2-inputs-f30-with-its-three-files'
    foreach ($f30 in @(@{ SiteChange = 'c'; SiteRevert = 'r' }, @{ SiteInstalled = 'i' }, @{ SiteChange = 'c'; SiteInstalled = 'i' })) {
        $bad = Test-IemHilV2Inputs -ReopenMaxMs 200 -FaultMaxUs 1000 @f30
        Assert ($bad.Count -eq 1 -and $bad[0] -like 'F30:*') "hil-v2-inputs-f30-needs-all-three [$((@($f30.Keys) | Sort-Object) -join ',')]"
    }
    foreach ($v in @(0, -1, [double]::NaN, [double]::PositiveInfinity)) {
        $bad = Test-IemHilV2Inputs -ReopenMaxMs $v -FaultMaxUs 1000
        Assert ($bad.Count -eq 1 -and $bad[0] -like 'ReopenMaxMs:*') "hil-v2-inputs-refuse-the-reopen-bound [$v]"
        $bad = Test-IemHilV2Inputs -ReopenMaxMs 200 -FaultMaxUs $v
        Assert ($bad.Count -eq 1 -and $bad[0] -like 'FaultMaxUs:*') "hil-v2-inputs-refuse-the-fault-bound [$v]"
    }

    # ---- hil-v1.ps1 end to end, against a stand-in for iemmode ----
    # The bundle layout (bundles\<sha>\ with the module next to the script); the
    # server is unreachable here (port 9), so the address checks fail.
    $hb = Join-Path $base 'hil'
    $hdir = Join-Path $hb "bundles\$S"
    New-Item -ItemType Directory -Force -Path $hdir | Out-Null
    Copy-Item -LiteralPath (Join-Path $here 'hil-v1.ps1'), (Join-Path $here 'IemPc.psm1'), (Join-Path $here 'IemHil.psm1') -Destination $hdir
    $fake = Join-Path $hb 'fake-iemmode.ps1'
    $fakeText = @'
# A stand-in for iemmode.exe (Test-IemHil.ps1): replies as scenario.json says and logs every call.
$sc = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'scenario.json')) | ConvertFrom-Json
$log = Join-Path $PSScriptRoot 'calls.log'
Add-Content -LiteralPath $log -Value ($args -join ' ')
$cmd = [string]$args[0]
$lines = @(Get-Content -LiteralPath $log)
$n = $lines.Count
if (@($sc.silent) -contains $cmd) { exit 4 }
# `cold` (optional): the guard while an engine comes up. After activate or inject-fault
# (a new engine) that many statuses show no engine, then as many show its first Status
# (frames 0, no callbacks); after force-reopen that many still show the old reset count.
$cold = 0
if ($null -ne $sc.PSObject.Properties['cold']) { $cold = [int]$sc.cold }
# `heard` (optional, default 1): after test-signal that many statuses show both spare
# outputs (engine.hil) at the asked level, the call's third argument, from the status
# `heard_from` (optional, default 1) on; the others silence. The engine's Status carries
# the peaks since its previous one, so a short TTL's signal may show only in a status
# that arrives after the TTL (heard_from above 1).
$heard = 1
if ($null -ne $sc.PSObject.Properties['heard']) { $heard = [int]$sc.heard }
$heardFrom = 1
if ($null -ne $sc.PSObject.Properties['heard_from']) { $heardFrom = [int]$sc.heard_from }
function Get-StatusesSince([string[]]$Marks) {
    # The status calls (this one included) after the last call named in $Marks; -1 without one.
    $count = 0
    for ($i = $lines.Count - 1; $i -ge 0; $i--) {
        $l = [string]$lines[$i]
        foreach ($m in $Marks) { if ($l -ceq $m -or $l.StartsWith($m + ' ')) { return $count } }
        if ($l -ceq 'status') { $count++ }
    }
    return -1
}
function Get-Knob([string]$Name, $Default) {
    # A scenario value (null included), else the default.
    if ($null -ne $sc.PSObject.Properties[$Name]) { return $sc.$Name }
    return $Default
}
# HIL v2 (#10). `alarms` (optional): the guard's alarms; every `alarm-test` adds the guard's
# test alarm with the next id (delivered: it fails when refused, as a push that reached no
# device), every `alarm-ack <id>` acknowledges one (refused for an id the guard does not
# hold). Every reply carries them, as the guard's does.
$alarms = New-Object System.Collections.ArrayList
$top = 0
foreach ($a in @(Get-Knob 'alarms' @())) {
    $o = [ordered]@{}
    foreach ($p in $a.PSObject.Properties) { $o[$p.Name] = $p.Value }
    [void]$alarms.Add($o)
    if ([int64]$a.id -gt $top) { $top = [int64]$a.id }
}
foreach ($l in $lines) {
    $l = [string]$l
    if ($l -ceq 'alarm-test') {
        $top++
        [void]$alarms.Add([ordered]@{ id = $top; at = 1; step = $null; text = 'alarm test (iemmode alarm-test)'; acked = $false; notified = $true; owner_question = $false })
    } elseif ($l.StartsWith('alarm-ack ')) {
        foreach ($o in $alarms) { if ([string]$o['id'] -ceq $l.Substring(10)) { $o['acked'] = $true } }
    }
}
$reopens = @($lines | Where-Object { $_ -eq 'force-reopen' }).Count
$mode = 'dev'
if ($sc.event_after -gt 0 -and $n -gt $sc.event_after) { $mode = 'event' }
$ok = -not (@($sc.refuse) -contains $cmd)
if ($cmd -eq 'alarm-ack' -and @($alarms | Where-Object { [string]$_['id'] -ceq [string]$args[1] }).Count -ne 1) { $ok = $false }
# `site` (optional): the installed site's path; `install-site <path>` writes the named
# file's bytes there, as the guard's install-site does.
if ($cmd -eq 'install-site' -and $ok -and $null -ne $sc.PSObject.Properties['site']) {
    [IO.File]::WriteAllBytes([string]$sc.site, [IO.File]::ReadAllBytes([string]$args[1]))
}
$reply = [ordered]@{ ok = $ok; mode = $mode; switching = $null; alarms = @($alarms.ToArray()); detail = ('fake ' + $cmd) }
$faults = @($lines | Where-Object { $_ -eq 'inject-fault' }).Count
if ($cmd -eq 'status') {
    $started = Get-StatusesSince @('activate', 'inject-fault')
    $reopened = Get-StatusesSince @('force-reopen')
    if ($reopened -ge 0 -and $reopened -le $cold) { $reopens-- }
    $last = $null
    if ($faults -gt 0) { $last = 70 }
    $frames = 32
    $callbacks = 3000 * $n
    if ($started -ge 0 -and $started -le 2 * $cold) { $frames = 0; $callbacks = 0 }
    $peak = 0.0
    $signals = @($lines | Where-Object { $_ -like 'test-signal *' })
    $sinceSignal = Get-StatusesSince @('test-signal')
    if ($signals.Count -gt 0 -and $sinceSignal -ge $heardFrom -and $sinceSignal -lt $heardFrom + $heard) {
        $asked = [double]::Parse(([string]$signals[$signals.Count - 1]).Split(' ')[2], [Globalization.CultureInfo]::InvariantCulture)
        $peak = [math]::Pow(10, $asked / 20)
    }
    # HIL v2's figures (#10): the engine's pid (a respawn is a new process), the pipe's
    # server (`server_pid`, default the pid), the last reopen's time once a reopen showed
    # (`reopen_us`, default 104000), the last fault's callback time (`fault_before`, default
    # null, until inject-fault; `fault_us`, default 412.5, after it).
    $enginePid = 4000 + $faults
    $server = Get-Knob 'server_pid' $enginePid
    $reopenUs = 0
    if ($reopens -gt 0) { $reopenUs = Get-Knob 'reopen_us' 104000 }
    $faultUs = Get-Knob 'fault_before' $null
    if ($faults -gt 0) { $faultUs = Get-Knob 'fault_us' 412.5 }
    if ($started -lt 0 -or $started -gt $cold) {
        $reply['engine'] = [ordered]@{ build = $sc.sha; frames = $frames; callbacks = $callbacks; missed = 0; resets = $reopens
                                       parked = $false; faulted = $false; pipe_private = $true; spawns = (1 + $faults); last_exit = $last
                                       hil = @([ordered]@{ tx = 94; peak = $peak }, [ordered]@{ tx = 95; peak = $peak })
                                       pid = $enginePid; pipe_server_pid = $server; last_reopen_us = $reopenUs; last_fault_us = $faultUs }
    }
}
Write-Output (ConvertTo-Json -InputObject $reply -Depth 5 -Compress)
if ($ok) { exit 0 }
exit 1
'@
    [IO.File]::WriteAllText($fake, $fakeText)
    function Invoke-HilRun([string]$scenario, [string]$branch = 'dev', [string]$ttl = '0.2', [string[]]$more = @()) {
        [IO.File]::WriteAllText((Join-Path $hb 'scenario.json'), $scenario)
        foreach ($f in @('calls.log', 'result.json')) { Remove-Item -LiteralPath (Join-Path $hb $f) -ErrorAction SilentlyContinue }
        & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File (Join-Path $hdir 'hil-v1.ps1') -Sha $S -Branch $branch `
            -JobRun 4242 -Out (Join-Path $hb 'result.json') -Iemmode $fake -Local 'http://127.0.0.1:9' -CardSeconds 1 -TestTtl $ttl @more | Out-Null
        $code = $LASTEXITCODE
        $calls = @()
        if (Test-Path -LiteralPath (Join-Path $hb 'calls.log')) { $calls = @(Get-Content -LiteralPath (Join-Path $hb 'calls.log')) }
        $res = Get-Content -LiteralPath (Join-Path $hb 'result.json') -Raw | ConvertFrom-Json
        return [pscustomobject]@{ exit = $code; result = $res; calls = $calls }
    }
    function CheckOk($res, [string]$name) { return @($res.checks | Where-Object { $_.name -eq $name -and $_.ok }).Count -eq 1 }
    function CheckFailed($res, [string]$name) { return @($res.checks | Where-Object { $_.name -eq $name -and -not $_.ok }).Count -eq 1 }
    function CheckDetail($res, [string]$name) { return (@($res.checks | Where-Object { $_.name -eq $name }) | ForEach-Object { $_.detail }) -join ' | ' }
    function CheckOf($res, [string]$name) { return @($res.checks | Where-Object { $_.name -eq $name })[0] }
    function Scenario([hashtable]$More = @{}) {
        # The default scenario with $More's keys added (HIL v2's knobs: alarms, site, fault_us ...).
        $s = [ordered]@{ sha = $S; refuse = @(); silent = @(); event_after = 0 }
        foreach ($k in $More.Keys) { $s[$k] = $More[$k] }
        return (ConvertTo-Json -InputObject $s -Depth 6 -Compress)
    }

    $h1 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":0}')
    $calls = $h1.calls
    Assert ($h1.exit -eq 1 -and $h1.result.conclusion -ceq 'failure' -and $h1.result.sha -ceq $S -and $h1.result.job_run -ceq '4242') "hil-run-with-the-server-down-fails (exit $($h1.exit))"
    Assert ($calls[0] -ceq 'job-begin 4242' -and $calls[1] -ceq "activate $S") "hil-run-begins-the-job-then-activates ($($calls -join ' | '))"
    Assert ($calls -contains 'test-signal mic1 -30 0.2' -and $calls -contains 'force-reopen' -and $calls -contains 'inject-fault' -and $calls -contains 'alarm-test') 'hil-run-drives-the-signal-reopen-fault-and-alarm'
    Assert ($calls[$calls.Count - 2] -ceq 'job-end 4242' -and $calls[$calls.Count - 1] -like "report $S red HIL v1 failure: *") 'hil-run-ends-the-job-then-reports-red'
    foreach ($n in @('activate', 'engine-build', 'card', 'pipes', 'test-signal', 'reopen', 'panic', 'alarm-push')) { Assert (CheckOk $h1.result $n) "hil-run-check-$n-passes" }
    foreach ($n in @('server-version', 'site-links', 'lan', 'public-host', 'tunnel-peer', 'lan-peer')) { Assert (CheckFailed $h1.result $n) "hil-run-check-$n-fails" }
    Assert ($h1.result.summary -ceq 'HIL v1 failure: server-version, site-links, lan, public-host, tunnel-peer, lan-peer (12 of 18 ok)') "hil-run-summary-names-checks-only ($($h1.result.summary))"
    # HIL v2 (#10): the engine serves its pipe, the reopen and fault times within their
    # bounds, the test alarm acknowledged (the guard's alarms hold only it: id 1).
    foreach ($n in @('pipe-owner', 'reopen-time', 'fault-time', 'alarm-ack')) {
        Assert (CheckOk $h1.result $n) "hil-run-v2-checks-pass-on-good-figures [$n] ($(CheckDetail $h1.result $n))"
    }
    $names = @($h1.result.checks | ForEach-Object { $_.name }) -join ','
    Assert ($names -ceq 'activate,engine-build,server-version,site-links,lan,public-host,tunnel-peer,lan-peer,card,pipes,pipe-owner,test-signal,reopen,reopen-time,panic,fault-time,alarm-push,alarm-ack') "hil-run-v2-checks-in-order ($names)"
    Assert ((CheckOf $h1.result 'pipe-owner').numbers.pid -eq 4000 -and (CheckOf $h1.result 'pipe-owner').numbers.pipe_server_pid -eq 4000) 'hil-run-pipe-owner-numbers-in-the-result'
    Assert ((CheckOf $h1.result 'reopen-time').numbers.reopen_ms -eq 104 -and (CheckOf $h1.result 'fault-time').numbers.fault_us -eq 412.5) 'hil-run-reopen-and-fault-times-in-the-result'
    $ack = CheckOf $h1.result 'alarm-ack'
    Assert ($ack.numbers.own -eq 1 -and @($ack.numbers.stale).Count -eq 0 -and @($calls | Where-Object { $_ -like 'alarm-ack *' }).Count -eq 1 -and $calls -contains 'alarm-ack 1') "hil-run-acks-its-own-test-alarm ($($ack.detail))"
    Assert ([array]::IndexOf($calls, 'alarm-ack 1') -gt [array]::IndexOf($calls, 'alarm-test')) 'hil-run-acks-after-the-push'
    $panic = @($h1.result.checks | Where-Object { $_.name -eq 'panic' })[0]
    Assert ($panic.numbers.spawns -eq 1 -and $panic.numbers.callbacks -gt 0) 'hil-run-panic-numbers-in-the-result'
    $card = @($h1.result.checks | Where-Object { $_.name -eq 'card' })[0]
    Assert ($card.numbers.frames -eq 32 -and $card.numbers.missed -eq 0 -and $card.numbers.resets -eq 0 -and $card.numbers.callbacks -ge 2850) 'hil-run-card-numbers-in-the-result'
    $ts = @($h1.result.checks | Where-Object { $_.name -eq 'test-signal' })[0]
    Assert ($ts.numbers.outputs -eq 2 -and $ts.numbers.after -eq 0 -and [math]::Abs($ts.numbers.lowest_dbfs + 30) -lt 0.01) "hil-run-test-signal-numbers-in-the-result ($($ts.detail))"

    # HIL's spare outputs that never reach the asked level, or still sound after the TTL
    # (the silence wait ends after -EngineWait), fail the test-signal check alone.
    $h11 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":0,"heard":0}') 'dev' '0.2' @('-EngineWait', '1')
    $d11 = CheckDetail $h11.result 'test-signal'
    Assert ((CheckFailed $h11.result 'test-signal') -and ($d11 -like '*peaked at -150.0 dBFS*')) "hil-run-a-signal-never-heard-fails ($d11)"
    $h12 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":0,"heard":1000}') 'dev' '0.2' @('-EngineWait', '1')
    $d12 = CheckDetail $h12.result 'test-signal'
    Assert ((CheckFailed $h12.result 'test-signal') -and ($d12 -like '*2 spare output(s) still sound after the TTL*')) "hil-run-a-signal-that-stays-fails ($d12)"
    foreach ($n in @('card', 'reopen', 'panic', 'alarm-push')) {
        foreach ($h in @($h11, $h12)) { Assert (CheckOk $h.result $n) "hil-run-a-failed-signal-leaves-$n ($(CheckDetail $h.result $n))" }
    }
    # A short TTL's signal may show only in a status that arrives after the TTL (the engine
    # sends Status about once a second, with the peaks since its previous one): every status
    # read before the silence that follows the signal counts, so the level on the third
    # status after test-signal, the first two silent, passes.
    $h13 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":0,"heard_from":3}')
    $d13 = CheckDetail $h13.result 'test-signal'
    Assert (CheckOk $h13.result 'test-signal') "hil-run-a-signal-heard-only-after-the-ttl-passes ($d13)"
    $ts13 = @($h13.result.checks | Where-Object { $_.name -eq 'test-signal' })[0]
    Assert ($ts13.numbers.outputs -eq 2 -and $ts13.numbers.after -eq 0 -and [math]::Abs($ts13.numbers.lowest_dbfs + 30) -lt 0.01) "hil-run-a-signal-heard-only-after-the-ttl-numbers ($d13)"

    $h2 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":["job-begin"],"silent":[],"event_after":0}')
    Assert ($h2.exit -eq 0 -and $h2.result.conclusion -ceq 'cancelled' -and $h2.result.summary -ceq 'HIL v1 cancelled: the PC was not free (job-begin refused)') "hil-run-a-refused-job-begin-is-cancelled ($($h2.result.summary))"
    Assert ($h2.result.why -like 'job-begin refused: fake job-begin*' -and $h2.result.summary -notlike '*fake*') 'hil-run-the-guards-detail-stays-in-why-never-in-the-public-summary'
    Assert ($h2.calls.Count -eq 1) "hil-run-a-refused-job-begin-touches-nothing-else ($($h2.calls -join ' | '))"

    $h3 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":3}')
    Assert ($h3.exit -eq 0 -and $h3.result.conclusion -ceq 'cancelled' -and $h3.result.summary -ceq 'HIL v1 cancelled: the guard left dev') "hil-run-a-switch-to-event-cancels (exit $($h3.exit), $($h3.result.summary))"
    Assert ($h3.result.why -like '*the guard left dev*' -and $h3.result.summary -notlike '*fake*') 'hil-run-a-cancelled-switch-keeps-its-text-in-why'
    Assert (-not (@($h3.calls) -like 'report *') -and (@($h3.calls) -contains 'job-end 4242') -and -not (@($h3.calls) -contains 'force-reopen')) "hil-run-a-cancelled-job-reports-nothing ($($h3.calls -join ' | '))"

    $h4 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":["job-begin"],"event_after":0}')
    Assert ($h4.exit -eq 1 -and $h4.result.conclusion -ceq 'failure' -and (CheckFailed $h4.result 'job-begin') -and $h4.calls.Count -eq 1) 'hil-run-an-unreachable-guard-fails'

    $h5 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":["report"],"silent":[],"event_after":0}')
    Assert ($h5.exit -eq 1 -and (CheckFailed $h5.result 'report')) 'hil-run-a-refused-report-is-a-failure'

    $h6 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":0}') 'feature'
    Assert ($h6.exit -eq 1 -and (CheckFailed $h6.result 'inputs') -and $h6.calls.Count -eq 0) 'hil-run-refuses-bad-inputs-before-any-call'

    $h8 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":0}') 'dev' '0'
    Assert ($h8.exit -eq 1 -and (CheckFailed $h8.result 'inputs') -and $h8.calls.Count -eq 0) 'hil-run-refuses-a-zero-ttl-before-any-call'

    $h7 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":["inject-fault"],"silent":[],"event_after":0}')
    Assert ($h7.exit -eq 1 -and (CheckFailed $h7.result 'panic') -and (CheckOk $h7.result 'alarm-push')) 'hil-run-a-refused-fault-injection-fails-the-panic-check'

    # The guard while the engine comes up (activate, the respawn after the fault): no
    # engine in its reply, then the engine's first Status (frames 0), and a forced
    # reopen's reset shows a few statuses late. HIL v1 waits for the engine it expects.
    $h9 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":0,"cold":2}')
    foreach ($n in @('activate', 'engine-build', 'card', 'pipes', 'test-signal', 'reopen', 'panic', 'alarm-push')) {
        Assert (CheckOk $h9.result $n) "hil-run-waits-for-the-engine-check-$n-passes ($(CheckDetail $h9.result $n))"
    }
    Assert ($h9.result.summary -ceq $h1.result.summary) "hil-run-waits-for-the-engine-same-summary ($($h9.result.summary))"

    # An engine that never shows: every wait ends after -EngineWait (-PanicWait for the
    # respawn) and its checks fail on what the guard showed.
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $h10 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":0,"cold":1000}') 'dev' '0.2' @('-EngineWait', '1', '-PanicWait', '1')
    $took = $clock.Elapsed.TotalSeconds
    $eb = CheckDetail $h10.result 'engine-build'
    Assert (($h10.exit -eq 1) -and (CheckFailed $h10.result 'engine-build') -and ($eb -ceq "engine build ''")) "hil-run-an-engine-that-never-shows-fails-engine-build ($eb)"
    foreach ($n in @('card', 'reopen', 'panic', 'pipe-owner', 'reopen-time', 'fault-time')) { Assert (CheckFailed $h10.result $n) "hil-run-an-engine-that-never-shows-fails-$n" }
    Assert ($took -lt 60) "hil-run-the-engine-waits-are-bounded ($took s)"

    # ---- HIL v2 end to end (#10) ----
    # alarm-ack: the guard holds an acknowledged test alarm (1), another alarm (2), an
    # unacknowledged test alarm from an earlier run (3) and the test text as an owner's
    # question (4); alarm-test raises 5. HIL acknowledges 5, then 3, never 4 (nor 1 or 2).
    $al = @([ordered]@{ id = 1; at = 1; step = $null; text = $T; acked = $true; notified = $true; owner_question = $false },
            [ordered]@{ id = 2; at = 1; step = 'engine_stop'; text = 'EngineStop: the engine did not stop'; acked = $false; notified = $true; owner_question = $true },
            [ordered]@{ id = 3; at = 1; step = $null; text = $T; acked = $false; notified = $true; owner_question = $false },
            [ordered]@{ id = 4; at = 1; step = $null; text = $T; acked = $false; notified = $true; owner_question = $true })
    $h14 = Invoke-HilRun (Scenario @{ alarms = $al })
    $acks = @($h14.calls | Where-Object { $_ -like 'alarm-ack *' }) -join ','
    Assert ($acks -ceq 'alarm-ack 5,alarm-ack 3') "hil-run-acks-its-own-test-alarm-and-the-stale-ones-never-another ($acks)"
    $ack = CheckOf $h14.result 'alarm-ack'
    Assert ($ack.ok -and $ack.numbers.own -eq 5 -and (@($ack.numbers.stale) -join ',') -ceq '3') "hil-run-alarm-ack-numbers-own-and-stale ($($ack.detail))"
    # A push that reached no device: the test alarm is acknowledged by no one (a later run
    # finds it stale), and no other alarm either.
    $h15 = Invoke-HilRun (Scenario @{ alarms = $al; refuse = @('alarm-test') })
    Assert ((CheckFailed $h15.result 'alarm-push') -and @($h15.calls | Where-Object { $_ -like 'alarm-ack *' }).Count -eq 0 -and $null -eq (CheckOf $h15.result 'alarm-ack')) "hil-run-acks-nothing-when-the-push-failed ($($h15.calls -join ' | '))"

    # fault-time: a respawned engine whose guard kept no fault time, or the time an earlier
    # fault left (unchanged by this injection), fails fault-time alone; panic still passes.
    $h16 = Invoke-HilRun (Scenario @{ fault_us = $null })
    $d16 = CheckDetail $h16.result 'fault-time'
    Assert ((CheckFailed $h16.result 'fault-time') -and $d16 -like "*lacks 'last_fault_us'*" -and (CheckOk $h16.result 'panic')) "hil-run-a-respawned-engine-without-last-fault-fails-fault-time ($d16)"
    $h17 = Invoke-HilRun (Scenario @{ fault_before = 412.5; fault_us = 412.5 })
    $d17 = CheckDetail $h17.result 'fault-time'
    Assert ((CheckFailed $h17.result 'fault-time') -and $d17 -like '*unchanged*' -and (CheckOk $h17.result 'panic')) "hil-run-an-earlier-faults-time-fails-fault-time ($d17)"
    # pipe-owner and reopen-time on bad figures: another pipe server, a slow reopen.
    $h18 = Invoke-HilRun (Scenario @{ server_pid = 1; reopen_us = 201000 })
    Assert ((CheckFailed $h18.result 'pipe-owner') -and (CheckFailed $h18.result 'reopen-time') -and (CheckOk $h18.result 'reopen')) "hil-run-another-pipe-server-and-a-slow-reopen-fail ($(CheckDetail $h18.result 'pipe-owner'); $(CheckDetail $h18.result 'reopen-time'))"
    $h19 = Invoke-HilRun (Scenario @{ reopen_us = 0 }) 'dev' '0.2' @('-ReopenMaxMs', '300')
    Assert ((CheckFailed $h19.result 'reopen-time') -and (CheckDetail $h19.result 'reopen-time') -like "*lacks 'last_reopen_us'*") "hil-run-an-engine-without-a-reopen-time-fails ($(CheckDetail $h19.result 'reopen-time'))"

    # F30: a synthetic change and its revert through install-site; the installed site's
    # bytes after the revert are the ones before (synthetic site text, no site value).
    $installed = Join-Path $hb 'installed-site.toml'
    $orig = "# synthetic site`n[engine]`nchannels = 8`n"
    $change = Join-Path $hb 'site-change.toml'
    $revert = Join-Path $hb 'site-revert.toml'
    $other = Join-Path $hb 'site-other.toml'
    [IO.File]::WriteAllText($change, $orig + "`n# hil f30 synthetic change, run 4242`n")
    [IO.File]::WriteAllText($revert, $orig)
    [IO.File]::WriteAllText($other, $orig.Replace('8', '9'))
    $f30 = @('-SiteChange', $change, '-SiteRevert', $revert, '-SiteInstalled', $installed)
    [IO.File]::WriteAllText($installed, $orig)
    $h20 = Invoke-HilRun (Scenario @{ site = $installed }) 'dev' '0.2' $f30
    $f = CheckOf $h20.result 'f30'
    Assert ($f.ok -and $f.numbers.restored -and [IO.File]::ReadAllText($installed) -ceq $orig) "hil-run-f30-restores-the-installed-site ($($f.detail))"
    Assert (([array]::IndexOf($h20.calls, "install-site $change") + 1) -eq [array]::IndexOf($h20.calls, "install-site $revert")) "hil-run-f30-changes-then-reverts ($($h20.calls -join ' | '))"
    [IO.File]::WriteAllText($installed, $orig)
    $h21 = Invoke-HilRun (Scenario @{ site = $installed }) 'dev' '0.2' @('-SiteChange', $change, '-SiteRevert', $other, '-SiteInstalled', $installed)
    Assert ((CheckFailed $h21.result 'f30') -and (CheckDetail $h21.result 'f30') -like '*differs*') "hil-run-f30-a-revert-that-differs-fails ($(CheckDetail $h21.result 'f30'))"
    # The change never reaches the installed site (a wrong -SiteInstalled): it proves nothing.
    [IO.File]::WriteAllText($installed, $orig)
    $h22 = Invoke-HilRun (Scenario @{}) 'dev' '0.2' $f30
    Assert ((CheckFailed $h22.result 'f30') -and (CheckDetail $h22.result 'f30') -like '*change*') "hil-run-f30-a-change-that-never-reached-the-installed-site-fails ($(CheckDetail $h22.result 'f30'))"
    $h23 = Invoke-HilRun (Scenario @{ site = $installed }) 'dev' '0.2' @('-SiteChange', $change, '-SiteRevert', $revert, '-SiteInstalled', (Join-Path $hb 'no-such-site.toml'))
    Assert ((CheckFailed $h23.result 'f30') -and @($h23.calls | Where-Object { $_ -like 'install-site *' }).Count -eq 0) "hil-run-f30-without-the-installed-site-fails ($(CheckDetail $h23.result 'f30'))"
    $h24 = Invoke-HilRun (Scenario @{}) 'dev' '0.2' @('-SiteChange', $change, '-SiteRevert', $revert)
    Assert ($h24.exit -eq 1 -and (CheckFailed $h24.result 'inputs') -and $h24.calls.Count -eq 0) 'hil-run-f30-needs-its-three-files-before-any-call'
} finally {
    Remove-Item -LiteralPath $base -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host 'Test-IemHil: all passed'
