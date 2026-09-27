#Requires -Version 5.1
<#
HIL v1 (S6 design note section 7). The ops repo's hil.yml `pc` job runs it from the
verified bundle directory (bundles\<sha>\) after `iemmode install`:

  hil-v1.ps1 -Sha <40 hex> -Branch dev|main -JobRun <run id> -Out <result.json>

Public, with no site value: it reads everything through `iemmode` and the
local server's /api/site, and never talks to GitHub.

Order: `iemmode job-begin` (the guard refuses unless dev, not switching, the
band quiet 5 min and the stage quiet 60 s) -> `iemmode activate` -> the checks
-> `iemmode job-end` -> `iemmode report <sha> green|red <summary>` ->
result.json {conclusion, summary, why, checks}. Once a switch to event started
(the guard left dev or runs a switch) the job ends as cancelled, never success,
and reports no result; a refused job-begin is cancelled too (the PC is not
free). The summary is public (the ops report job posts it as the hil/iem-pc
check run, P6): check names, counts and fixed cancel phrases only; the guard's
own text goes to `why` and the checks' details, both private.
Exit 0 for success or cancelled, 1 for failure (the report job posts the
conclusion from result.json).

The engine checks read `engine` from the `iemmode status` reply: build (the
bundle SHA), frames (measured), callbacks, missed, resets, parked, faulted and
pipe_private (the engine pipes' DACL holds only the user and SYSTEM). A check
whose data is missing fails. The RT-panic check fails until the guard can
inject a panic (no iemmode command for it yet). F30 runs when -SiteChange and
-SiteRevert name the synthetic site change and its revert.
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
    [string]$SiteChange = '',
    [string]$SiteRevert = ''
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'IemPc.psm1') -Force

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

function Invoke-HilChecks {
    $r = Invoke-Hil -A @('activate', $Sha)
    if ($script:cancelled) { return }
    if (-not (Test-IemModeOk -Result $r)) { Add-HilCheck 'activate' $false (Get-IemModeText -Result $r); return }
    Add-HilCheck 'activate' $true ('bundle {0} active' -f $Sha)

    # Versions: the engine's build and the server's /api/version name this SHA.
    $st = Get-HilStatus
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
    Invoke-HilUrlCheck -Name 'lan' -Base ([string](Get-IemProp $site 'lan_url'))
    $public = [string](Get-IemProp $site 'public_host')
    if ($public -and $public -notmatch '^https?://') { $public = 'https://' + $public }
    Invoke-HilUrlCheck -Name 'public-host' -Base $public

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

    # The card-masked test signal (the guard masks it to [guard] hil_tx and proves
    # the per-TX routing from the engine's meters within its TTL).
    $r = Invoke-Hil -A @('test-signal', $TestInput, $TestDbfs.ToString($inv), $TestTtl.ToString($inv))
    if ($script:cancelled) { return }
    $sent = Test-IemModeOk -Result $r
    $detail = Get-IemModeText -Result $r
    Start-Sleep -Milliseconds ([int](($TestTtl + 1) * 1000))
    $after = Get-HilStatus
    if ($script:cancelled) { return }
    $faulted = Get-IemProp (Get-IemProp $after 'engine') 'faulted'
    Add-HilCheck 'test-signal' ($sent -and $faulted -eq $false) ('{0}; faulted after the TTL: {1}' -f $detail, $faulted)

    # A forced reopen: one more reset, the card back at 32 and streaming.
    $before = Get-HilStatus
    if ($script:cancelled) { return }
    $r = Invoke-Hil -A @('force-reopen')
    if ($script:cancelled) { return }
    $reopened = Test-IemModeOk -Result $r
    Start-Sleep -Seconds 2
    $after = Get-HilStatus
    if ($script:cancelled) { return }
    $ro = Test-IemHilReopen -Before (Get-IemProp $before 'engine') -After (Get-IemProp $after 'engine')
    Add-HilCheck 'reopen' ($reopened -and $ro.ok) ('{0}; {1}' -f (Get-IemModeText -Result $r), $ro.detail) $ro.numbers

    # RT panic -> exit 70, release, respawn, fade-in (design section 7).
    Add-HilCheck 'panic' $false 'no iemmode command injects an RT panic yet (the guard protocol has no fault request)'

    # The alarm push to the alarm recipients.
    $r = Invoke-Hil -A @('alarm-test')
    if ($script:cancelled) { return }
    Add-HilCheck 'alarm-push' (Test-IemModeOk -Result $r) (Get-IemModeText -Result $r)

    # F30: a synthetic site change and its revert.
    if ($SiteChange -or $SiteRevert) {
        if (-not ($SiteChange -and $SiteRevert)) { Add-HilCheck 'f30' $false 'F30 needs both -SiteChange and -SiteRevert'; return }
        $r1 = Invoke-Hil -A @('install-site', $SiteChange)
        if ($script:cancelled) { return }
        $r2 = Invoke-Hil -A @('install-site', $SiteRevert)
        if ($script:cancelled) { return }
        Add-HilCheck 'f30' ((Test-IemModeOk -Result $r1) -and (Test-IemModeOk -Result $r2)) `
            ('change: {0}; revert: {1}' -f (Get-IemModeText -Result $r1), (Get-IemModeText -Result $r2))
    }
}

$started = (Get-Date).ToUniversalTime().ToString('o')
$begun = $false
$problems = Test-IemHilInputs -Sha $Sha -Branch $Branch -JobRun $JobRun -Out $Out -ScriptDir $PSScriptRoot
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
