#Requires -Version 5.1
# Self-test of hil-v1.ps1 (HIL v1, #9; v2, #10) on Windows PowerShell 5.1 (CI
# job windows, an ephemeral administrator runner): the script end to end
# against a stand-in for iemmode, from a temp bundles\<sha>\ folder.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
Import-Module (Join-Path $here 'IemPc.psm1') -Force
function Assert($cond, $what) { if (-not $cond) { throw "FAILED: $what" } ; Write-Host "ok  $what" }

$id = [guid]::NewGuid().ToString('N').Substring(0, 8)
$base = Join-Path ([IO.Path]::GetTempPath()) ('iem-hil-test-' + $id)
$S = '0123456789abcdef0123456789abcdef01234567'
New-Item -ItemType Directory -Force -Path $base | Out-Null

try {
    # ---- hil-v1.ps1 end to end, against a stand-in for iemmode ----
    # The bundle layout (bundles\<sha>\ with the module next to the script); the
    # server is unreachable here (port 9), so the address checks fail.
    $hb = Join-Path $base 'hil'
    $hdir = Join-Path $hb "bundles\$S"
    New-Item -ItemType Directory -Force -Path $hdir | Out-Null
    Copy-Item -LiteralPath (Join-Path $here 'hil-v1.ps1'), (Join-Path $here 'IemPc.psm1') -Destination $hdir
    $fake = Join-Path $hb 'fake-iemmode.ps1'
    $fakeText = @'
# A stand-in for iemmode.exe (Test-IemPc.ps1): replies as scenario.json says and logs every call.
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
$reopens = @($lines | Where-Object { $_ -eq 'force-reopen' }).Count
$mode = 'dev'
if ($sc.event_after -gt 0 -and $n -gt $sc.event_after) { $mode = 'event' }
$ok = -not (@($sc.refuse) -contains $cmd)
$reply = [ordered]@{ ok = $ok; mode = $mode; switching = $null; alarms = @(); detail = ('fake ' + $cmd) }
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
    if ($started -lt 0 -or $started -gt $cold) {
        $reply['engine'] = [ordered]@{ build = $sc.sha; frames = $frames; callbacks = $callbacks; missed = 0; resets = $reopens
                                       parked = $false; faulted = $false; pipe_private = $true; spawns = (1 + $faults); last_exit = $last
                                       hil = @([ordered]@{ tx = 94; peak = $peak }, [ordered]@{ tx = 95; peak = $peak }) }
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

    $h1 = Invoke-HilRun ('{"sha":"' + $S + '","refuse":[],"silent":[],"event_after":0}')
    $calls = $h1.calls
    Assert ($h1.exit -eq 1 -and $h1.result.conclusion -ceq 'failure' -and $h1.result.sha -ceq $S -and $h1.result.job_run -ceq '4242') "hil-run-with-the-server-down-fails (exit $($h1.exit))"
    Assert ($calls[0] -ceq 'job-begin 4242' -and $calls[1] -ceq "activate $S") "hil-run-begins-the-job-then-activates ($($calls -join ' | '))"
    Assert ($calls -contains 'test-signal mic1 -30 0.2' -and $calls -contains 'force-reopen' -and $calls -contains 'inject-fault' -and $calls -contains 'alarm-test') 'hil-run-drives-the-signal-reopen-fault-and-alarm'
    Assert ($calls[$calls.Count - 2] -ceq 'job-end 4242' -and $calls[$calls.Count - 1] -like "report $S red HIL v1 failure: *") 'hil-run-ends-the-job-then-reports-red'
    foreach ($n in @('activate', 'engine-build', 'card', 'pipes', 'test-signal', 'reopen', 'panic', 'alarm-push')) { Assert (CheckOk $h1.result $n) "hil-run-check-$n-passes" }
    foreach ($n in @('server-version', 'site-links', 'lan', 'public-host')) { Assert (CheckFailed $h1.result $n) "hil-run-check-$n-fails" }
    Assert ($h1.result.summary -ceq 'HIL v1 failure: server-version, site-links, lan, public-host (8 of 12 ok)') "hil-run-summary-names-checks-only ($($h1.result.summary))"
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
    foreach ($n in @('card', 'reopen', 'panic')) { Assert (CheckFailed $h10.result $n) "hil-run-an-engine-that-never-shows-fails-$n" }
    Assert ($took -lt 60) "hil-run-the-engine-waits-are-bounded ($took s)"
} finally {
    Remove-Item -LiteralPath $base -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host 'Test-IemHil: all passed'
