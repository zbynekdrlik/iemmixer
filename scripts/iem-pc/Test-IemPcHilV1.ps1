#Requires -Version 5.1
# Self-test of HIL v1's helpers (IemPcHilV1.psm1, which IemPc.psm1 imports;
# pure, plus iemmode that does not start) on Windows PowerShell 5.1 (CI job
# windows; split out of Test-IemPc.ps1, #36). hil-v1.ps1 end to end is
# Test-IemHil.ps1's.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
Import-Module (Join-Path $here 'IemPc.psm1') -Force
function Assert($cond, $what) { if (-not $cond) { throw "FAILED: $what" } ; Write-Host "ok  $what" }
function Throws([scriptblock]$b, $what) { $t = $false; try { & $b } catch { $t = $true }; Assert $t $what }
function ErrorOf([scriptblock]$b) { try { & $b; return '' } catch { return "$_" } }
function Sorted($items) { return ((@($items) | ForEach-Object { "$_" } | Sort-Object) -join ',') }

$id = [guid]::NewGuid().ToString('N').Substring(0, 8)
$base = Join-Path ([IO.Path]::GetTempPath()) ('iempc-hil-test-' + $id)

try {
    # ---- HIL helpers (pure) ----
    $S = '0123456789abcdef0123456789abcdef01234567'
    Assert ((Test-IemHilVersion -Sha $S -Version ([pscustomobject]@{ git_hash = '0123456' })).ok) 'hil-version-a-short-hash-prefix'
    Assert ((Test-IemHilVersion -Sha $S -Version ([pscustomobject]@{ git_hash = $S })).ok) 'hil-version-the-full-sha'
    foreach ($v in @('012345', '1123456', '0123456789ABCDEF', '')) {
        Assert (-not (Test-IemHilVersion -Sha $S -Version ([pscustomobject]@{ git_hash = $v })).ok) "hil-version-refuses [$v]"
    }
    Assert (-not (Test-IemHilVersion -Sha $S -Version ([pscustomobject]@{ version = '2.0.0' })).ok) 'hil-version-refuses-a-missing-hash'
    Assert (-not (Test-IemHilVersion -Sha $S -Version $null).ok) 'hil-version-refuses-no-answer'

    function Eng($cb, $missed, $resets, $frames = 32, $parked = $false, $faulted = $false) {
        [pscustomobject]@{ frames = $frames; callbacks = $cb; missed = $missed; resets = $resets; parked = $parked; faulted = $faulted }
    }
    $c = Test-IemHilCard -Before (Eng 1000 2 1) -After (Eng 343000 2 1) -Seconds 120
    Assert ($c.ok -and $c.numbers.callbacks -eq 342000 -and $c.numbers.missed -eq 0 -and $c.numbers.resets -eq 0) 'hil-card-passes-at-the-floor'
    Assert (-not (Test-IemHilCard -Before (Eng 1000 2 1) -After (Eng 342999 2 1) -Seconds 120).ok) 'hil-card-refuses-one-below-the-floor'
    Assert (-not (Test-IemHilCard -Before (Eng 0 0 0) -After (Eng 360000 1 0) -Seconds 120).ok) 'hil-card-refuses-one-missed-period'
    Assert (-not (Test-IemHilCard -Before (Eng 0 0 0) -After (Eng 360000 0 1) -Seconds 120).ok) 'hil-card-refuses-one-reset'
    Assert (-not (Test-IemHilCard -Before (Eng 0 0 0) -After (Eng 360000 0 0 64) -Seconds 120).ok) 'hil-card-refuses-a-measured-64'
    Assert (-not (Test-IemHilCard -Before (Eng 0 0 0) -After (Eng 360000 0 0 32 $true) -Seconds 120).ok) 'hil-card-refuses-parked'
    Assert (-not (Test-IemHilCard -Before (Eng 0 0 0) -After (Eng 360000 0 0 32 $false $true) -Seconds 120).ok) 'hil-card-refuses-faulted'
    $gap = Test-IemHilCard -Before (Eng 0 0 0) -After ([pscustomobject]@{ frames = 32; callbacks = 360000 }) -Seconds 120
    Assert (-not $gap.ok -and $gap.detail -like "*lacks 'missed'*") 'hil-card-refuses-a-status-without-its-fields'
    Assert ((Test-IemHilCard -Before $null -After (Eng 0 0 0) -Seconds 1).detail -like '*no engine status*') 'hil-card-refuses-no-engine-status'
    Assert ((Test-IemHilReopen -Before (Eng 100 0 2) -After (Eng 200 0 3)).ok) 'hil-reopen-one-reset'
    Assert (-not (Test-IemHilReopen -Before (Eng 100 0 2) -After (Eng 200 0 2)).ok) 'hil-reopen-refuses-no-reset'
    Assert (-not (Test-IemHilReopen -Before (Eng 100 0 2) -After (Eng 200 0 4)).ok) 'hil-reopen-refuses-two-resets'
    Assert (-not (Test-IemHilReopen -Before (Eng 100 0 2) -After (Eng 100 0 3)).ok) 'hil-reopen-refuses-a-stalled-card'
    Assert (-not (Test-IemHilReopen -Before (Eng 100 0 2) -After (Eng 200 0 3 64)).ok) 'hil-reopen-refuses-a-measured-64'
    function EngP($cb, $spawns, $last, $frames = 32, $parked = $false, $faulted = $false) {
        [pscustomobject]@{ frames = $frames; callbacks = $cb; missed = 0; resets = 0; parked = $parked; faulted = $faulted; spawns = $spawns; last_exit = $last }
    }
    $p0 = EngP 5000 1 $null
    $pOk = Test-IemHilPanic -Before $p0 -Respawned (EngP 10 2 70) -Later (EngP 11 2 70)
    Assert ($pOk.ok -and $pOk.numbers.spawns -eq 1 -and $pOk.numbers.callbacks -eq 1) 'hil-panic-exit-70-one-respawn-streaming'
    Assert (-not (Test-IemHilPanic -Before $p0 -Respawned (EngP 10 2 70) -Later (EngP 10 2 70)).ok) 'hil-panic-refuses-a-new-engine-that-does-not-stream'
    Assert (-not (Test-IemHilPanic -Before $p0 -Respawned (EngP 10 2 1) -Later (EngP 6010 2 1)).ok) 'hil-panic-refuses-another-exit-code'
    Assert (-not (Test-IemHilPanic -Before $p0 -Respawned (EngP 10 2 $null) -Later (EngP 6010 2 $null)).ok) 'hil-panic-refuses-no-exit-code'
    Assert (-not (Test-IemHilPanic -Before $p0 -Respawned (EngP 10 3 70) -Later (EngP 6010 3 70)).ok) 'hil-panic-refuses-two-respawns'
    Assert (-not (Test-IemHilPanic -Before $p0 -Respawned (EngP 10 2 70) -Later (EngP 6010 2 70 64)).ok) 'hil-panic-refuses-a-measured-64'
    Assert (-not (Test-IemHilPanic -Before $p0 -Respawned (EngP 10 2 70) -Later (EngP 6010 2 70 32 $true)).ok) 'hil-panic-refuses-parked'
    Assert (-not (Test-IemHilPanic -Before $p0 -Respawned (EngP 10 2 70) -Later (EngP 6010 2 70 32 $false $true)).ok) 'hil-panic-refuses-faulted'
    $pn = Test-IemHilPanic -Before $p0 -Respawned $null -Later $null
    Assert (-not $pn.ok -and $pn.detail -ceq 'no new engine within the wait') 'hil-panic-refuses-no-respawn'
    $pg = Test-IemHilPanic -Before ([pscustomobject]@{ frames = 32 }) -Respawned (EngP 10 2 70) -Later (EngP 6010 2 70)
    Assert (-not $pg.ok -and $pg.detail -like "*lacks 'spawns'*") 'hil-panic-refuses-a-status-without-spawns'
    # What HIL v1's waits look for: the guard shows no engine until its hello and first
    # Status, and that first Status may come before the card streams (frames 0).
    function EngU($cb, $frames = 32, $resets = 2, $build = $S) {
        [pscustomobject]@{ build = $build; frames = $frames; callbacks = $cb; resets = $resets }
    }
    Assert (Test-IemHilEngineUp -Engine (EngU 3000)) 'hil-engine-up-streaming-at-32'
    Assert (-not (Test-IemHilEngineUp -Engine $null)) 'hil-engine-up-refuses-no-engine'
    Assert (-not (Test-IemHilEngineUp -Engine (EngU 0 0 0))) 'hil-engine-up-refuses-the-first-status-before-the-card'
    Assert (-not (Test-IemHilEngineUp -Engine (EngU 3000 64))) 'hil-engine-up-refuses-a-measured-64'
    Assert (-not (Test-IemHilEngineUp -Engine ([pscustomobject]@{ frames = 32 }))) 'hil-engine-up-refuses-a-status-without-callbacks'
    Assert (Test-IemHilEngineUp -Engine (EngU 3000) -Sha $S) 'hil-engine-up-names-the-bundle'
    Assert (-not (Test-IemHilEngineUp -Engine (EngU 3000 32 2 ('f' + $S.Substring(1))) -Sha $S)) 'hil-engine-up-refuses-another-build'
    Assert (-not (Test-IemHilEngineUp -Engine (EngU 3000 32 2 '') -Sha $S)) 'hil-engine-up-refuses-an-empty-build'
    Assert (Test-IemHilEngineUp -Engine (EngU 3000) -Callbacks 2999) 'hil-engine-up-callbacks-above'
    Assert (-not (Test-IemHilEngineUp -Engine (EngU 3000) -Callbacks 3000)) 'hil-engine-up-refuses-callbacks-not-above'
    Assert (Test-IemHilEngineUp -Engine (EngU 3000) -Resets 1) 'hil-engine-up-resets-above'
    Assert (-not (Test-IemHilEngineUp -Engine (EngU 3000) -Resets 2)) 'hil-engine-up-refuses-resets-not-above'
    Assert (-not (Test-IemHilEngineUp -Engine ([pscustomobject]@{ frames = 32; callbacks = 3000 }) -Resets 0)) 'hil-engine-up-refuses-a-status-without-resets'
    # The test signal on HIL's spare outputs (engine.hil: each spare card output and its
    # peak since the engine's previous Status; #9, 2026-09-28).
    function EngH([double[]]$peaks) {
        $outs = @()
        $tx = 94
        foreach ($p in $peaks) { $outs += [pscustomobject]@{ tx = $tx; peak = $p }; $tx++ }
        [pscustomobject]@{ frames = 32; callbacks = 3000; faulted = $false; hil = $outs }
    }
    $lvl = [math]::Pow(10, -30 / 20)
    Assert (Test-IemHilSilent -Engine (EngH @(0, 0))) 'hil-silent-every-spare-output-at-zero'
    Assert (-not (Test-IemHilSilent -Engine (EngH @(0, 0.001)))) 'hil-silent-refuses-one-that-sounds'
    Assert (-not (Test-IemHilSilent -Engine (EngH @()))) 'hil-silent-refuses-no-spare-output'
    Assert (-not (Test-IemHilSilent -Engine (EngU 3000))) 'hil-silent-refuses-a-status-without-hil'
    Assert (-not (Test-IemHilSilent -Engine $null)) 'hil-silent-refuses-no-engine'
    # A status that carried the signal on some spare output: HIL v1's silence wait takes a
    # silent status as "after" only once one of these was read.
    Assert (Test-IemHilHeard -Engine (EngH @(0, 0.001))) 'hil-heard-one-spare-output-that-sounds'
    Assert (Test-IemHilHeard -Engine (EngH @($lvl, $lvl))) 'hil-heard-every-spare-output-at-the-level'
    Assert (-not (Test-IemHilHeard -Engine (EngH @(0, 0)))) 'hil-heard-refuses-silence'
    Assert (-not (Test-IemHilHeard -Engine (EngH @()))) 'hil-heard-refuses-no-spare-output'
    Assert (-not (Test-IemHilHeard -Engine (EngU 3000))) 'hil-heard-refuses-a-status-without-hil'
    Assert (-not (Test-IemHilHeard -Engine $null)) 'hil-heard-refuses-no-engine'
    $quietAfter = EngH @(0, 0)
    $sg = Test-IemHilSignal -During @((EngH @(0, 0)), (EngH @($lvl, ($lvl * 0.99))), $null) -After $quietAfter -Dbfs (-30)
    Assert ($sg.ok -and $sg.numbers.outputs -eq 2 -and $sg.numbers.after -eq 0 -and [math]::Abs($sg.numbers.highest_dbfs + 30) -lt 1e-9) "hil-signal-heard-at-the-level-then-silent ($($sg.detail))"
    Assert ($sg.detail -ceq '2 spare output(s) at -30.1 dBFS (asked -30.0), silent after the TTL') "hil-signal-detail ($($sg.detail))"
    $sg = Test-IemHilSignal -During @((EngH @($lvl, 0))) -After $quietAfter -Dbfs (-30)
    Assert (-not $sg.ok -and $sg.detail -like '*peaked at -150.0 dBFS*') "hil-signal-refuses-a-spare-output-never-heard ($($sg.detail))"
    foreach ($p in @(($lvl * 0.9), ($lvl * 1.1), 0.1)) {
        $sg = Test-IemHilSignal -During @((EngH @($p, $p))) -After $quietAfter -Dbfs (-30)
        Assert (-not $sg.ok) "hil-signal-refuses-another-level [$p]"
    }
    $sg = Test-IemHilSignal -During @((EngH @($lvl, $lvl))) -After (EngH @(0, $lvl)) -Dbfs (-30)
    Assert (-not $sg.ok -and $sg.numbers.after -eq 1 -and $sg.detail -like '*1 spare output(s) still sound after the TTL*') "hil-signal-refuses-one-still-sounding ($($sg.detail))"
    Assert (-not (Test-IemHilSignal -During @() -After $quietAfter -Dbfs (-30)).ok) 'hil-signal-refuses-no-status-during-the-ttl'
    Assert ((Test-IemHilSignal -During @((EngH @($lvl))) -After (EngU 3000) -Dbfs (-30)).detail -ceq "the engine status lacks 'hil'") 'hil-signal-refuses-a-status-without-hil'
    Assert ((Test-IemHilSignal -During @() -After (EngH @()) -Dbfs (-30)).detail -ceq 'the engine opened no HIL output ([guard] hil_tx)') 'hil-signal-refuses-an-engine-without-spare-outputs'

    $ok1 = New-IemHilCheck -Name 'a' -Ok $true
    $bad1 = New-IemHilCheck -Name 'b' -Ok $false -Detail 'no'
    Assert ((Get-IemHilConclusion -Checks @($ok1, $ok1)) -ceq 'success') 'hil-conclusion-success'
    Assert ((Get-IemHilConclusion -Checks @($ok1, $bad1)) -ceq 'failure') 'hil-conclusion-a-failed-check-fails'
    Assert ((Get-IemHilConclusion -Checks @()) -ceq 'failure') 'hil-conclusion-no-check-fails'
    Assert ((Get-IemHilConclusion -Checks @($ok1) -Cancelled) -ceq 'cancelled') 'hil-conclusion-cancelled-wins'
    Assert ((Get-IemHilSummary -Conclusion 'failure' -Checks @($ok1, $bad1)) -ceq 'HIL v1 failure: b (1 of 2 ok)') 'hil-summary-names-the-failed-checks-only'
    Assert ((Get-IemHilSummary -Conclusion 'success' -Checks @($ok1, $ok1)) -ceq 'HIL v1 success: 2 checks ok') 'hil-summary-success'
    # The summary is public (hil/iem-pc on the public repo, P6): a cancelled
    # job names a fixed reason, never the guard's free text.
    Assert ((Get-IemHilSummary -Conclusion 'cancelled' -Reason 'left-dev') -ceq 'HIL v1 cancelled: the guard left dev') 'hil-summary-cancelled-left-dev-is-a-fixed-phrase'
    Assert ((Get-IemHilSummary -Conclusion 'cancelled' -Reason 'not-free') -ceq 'HIL v1 cancelled: the PC was not free (job-begin refused)') 'hil-summary-cancelled-not-free-is-a-fixed-phrase'
    Throws { Get-IemHilSummary -Conclusion 'cancelled' -Reason 'input mic7 refused in C:\Users\member1' } 'hil-summary-cancelled-takes-a-reason-code-never-text'
    Assert ((Get-IemHilSummary -Conclusion 'failure' -Checks @($ok1, $bad1) -Reason 'left-dev') -ceq 'HIL v1 failure: b (1 of 2 ok)') 'hil-summary-a-reason-never-changes-a-failure'

    Assert ((ConvertFrom-IemReply -Text '{"ok":true,"mode":"dev"}').mode -ceq 'dev') 'reply-compact'
    Assert ((ConvertFrom-IemReply -Text "{`n  `"ok`": false,`n  `"mode`": `"event`"`n}").mode -ceq 'event') 'reply-indented'
    Assert ((ConvertFrom-IemReply -Text "a log line`n{`"ok`":true,`"mode`":`"dev`"}").ok -eq $true) 'reply-after-a-log-line'
    foreach ($t in @('', '   ', '[1,2]', '42', 'not json')) { Assert ($null -eq (ConvertFrom-IemReply -Text $t)) "reply-refuses [$t]" }
    Assert (-not (Test-IemHilSwitchStarted -Reply ([pscustomobject]@{ mode = 'dev'; switching = $null }))) 'switch-dev-idle-is-not-a-switch'
    Assert (Test-IemHilSwitchStarted -Reply ([pscustomobject]@{ mode = 'event'; switching = $null })) 'switch-event-mode'
    Assert (Test-IemHilSwitchStarted -Reply ([pscustomobject]@{ mode = 'dev'; switching = [pscustomobject]@{ to = 'event' } })) 'switch-running'
    Assert (-not (Test-IemHilSwitchStarted -Reply $null)) 'switch-unknown-without-a-reply'
    $hilDir = Join-Path $base "bundles\$S"
    Assert ((Test-IemHilInputs -Sha $S -Branch 'dev' -JobRun '4242' -Out 'r.json' -ScriptDir $hilDir).Count -eq 0) 'hil-inputs-valid'
    Assert ((Test-IemHilInputs -Sha $S -Branch 'feature' -JobRun '42a' -Out '' -ScriptDir (Join-Path $base 'elsewhere')).Count -eq 4) 'hil-inputs-branch-run-out-and-place'
    Assert ((Test-IemHilInputs -Sha $S.ToUpperInvariant() -Branch 'main' -JobRun '1' -Out 'r.json' -ScriptDir $hilDir).Count -eq 2) 'hil-inputs-an-uppercase-sha-and-its-folder'
    $hi = @{ Sha = $S; Branch = 'dev'; JobRun = '1'; Out = 'r.json'; ScriptDir = $hilDir }
    Assert ((Test-IemHilInputs @hi -TestDbfs (-20) -TestTtl 60).Count -eq 0) 'hil-inputs-the-test-signal-at-its-limits'
    Assert ((Test-IemHilInputs @hi -TestDbfs (-120) -TestTtl 0.001).Count -eq 0) 'hil-inputs-a-quiet-short-test-signal'
    foreach ($db in @(-19.9, 0, 6, [double]::NaN, [double]::PositiveInfinity, [double]::NegativeInfinity)) {
        $bad = Test-IemHilInputs @hi -TestDbfs $db -TestTtl 10
        Assert ($bad.Count -eq 1 -and $bad[0] -like 'TestDbfs:*') "hil-inputs-refuse-the-level [$db]"
    }
    foreach ($ttl in @(0, -1, 60.001, [double]::NaN)) {
        $bad = Test-IemHilInputs @hi -TestDbfs (-30) -TestTtl $ttl
        Assert ($bad.Count -eq 1 -and $bad[0] -like 'TestTtl:*') "hil-inputs-refuse-the-ttl [$ttl]"
    }
    $m = Invoke-IemMode -Exe (Join-Path $base 'no-such-iemmode.exe') -Arguments @('status')
    Assert ($null -eq $m.exit -and $null -eq $m.reply -and -not (Test-IemModeOk -Result $m)) 'iemmode-that-does-not-start'
} finally {
    Remove-Item -LiteralPath $base -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host 'Test-IemPcHilV1: all passed'
