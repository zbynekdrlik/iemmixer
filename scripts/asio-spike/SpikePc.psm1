#Requires -Version 5.1
# S1a ASIO spike: PC-side work (design note §5). Never ends a process by
# force; never starts REAPER while the spike runs or before the driver's
# preferred buffer is back at its recorded value.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
# Get-GoldenAsioHolders, Write-GoldenStatus, Write-GoldenRequest, Test-GoldenHttp,
# Wait-GoldenProcessGone, Invoke-GoldenSaveQuit, Get-GoldenMeterSamples (S1b, reviewed).
Import-Module (Join-Path $PSScriptRoot 'GoldenPc.psm1') -Force -Global

$script:SpikeFrames = @(32, 48, 64)
$script:SpikeTaskPath = '\iemmixer\'
$script:SpikeTaskName = 'iemmixer-asio-spike'

function Get-SpikeBufferPref {
    param([Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][string]$Name)
    $item = Get-Item -LiteralPath $Key
    $kind = $item.GetValueKind($Name)
    if (@([Microsoft.Win32.RegistryValueKind]::DWord, [Microsoft.Win32.RegistryValueKind]::String) -notcontains $kind) {
        throw "$Name has registry kind $kind (expected DWord or String)"
    }
    $raw = [string]$item.GetValue($Name)
    [pscustomobject]@{ value = [int]$raw; kind = "$kind"; raw = $raw }
}

function Set-SpikeBufferPref {
    # -Raw: the original text of a String value (recorded at preflight), written back byte for byte.
    param([Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][string]$Name,
          [Parameter(Mandatory)][int]$Value, [Parameter(Mandatory)][int]$Original, [string]$Raw = '')
    if (($script:SpikeFrames -notcontains $Value) -and ($Value -ne $Original)) {
        throw "buffer $Value refused: only 32, 48, 64 or the recorded original $Original"
    }
    $before = Get-SpikeBufferPref -Key $Key -Name $Name
    $data = if ($before.kind -eq 'String') { "$Value" } else { $Value }
    if ($Raw) {
        if ($before.kind -ne 'String' -or [int]$Raw -ne $Value) { throw "text '$Raw' refused for $Value ($($before.kind))" }
        $data = $Raw
    }
    Set-ItemProperty -LiteralPath $Key -Name $Name -Value $data -Type $before.kind
    $after = Get-SpikeBufferPref -Key $Key -Name $Name
    if ($after.value -ne $Value -or $after.kind -ne $before.kind -or ($Raw -and $after.raw -ne $Raw)) {
        throw "read-back '$($after.raw)' ($($after.kind)) after writing '$data' ($($before.kind))"
    }
    [pscustomobject]@{ before = $before.value; after = $after.value; kind = $after.kind; raw = $after.raw }
}

function Test-SpikeSums {
    # SHA256SUMS lines: "<64 hex>  <file name>", names without any path part.
    param([Parameter(Mandatory)][string]$Bin)
    $lines = @(Get-Content -LiteralPath (Join-Path $Bin 'SHA256SUMS') | Where-Object { $_.Trim() })
    if ($lines.Count -eq 0) { throw 'SHA256SUMS is empty' }
    $names = @()
    foreach ($line in $lines) {
        if ($line -notmatch '^([0-9a-f]{64})  ([A-Za-z0-9_.-]+)$') { throw "malformed SHA256SUMS line: $line" }
        $sha = $Matches[1]; $file = $Matches[2]
        $actual = (Get-FileHash -LiteralPath (Join-Path $Bin $file) -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $sha) { throw "hash mismatch: $file" }
        $names += $file
    }
    return ,$names
}

function Get-SpikeBlockers {
    # I3: one ASIO host. Empty = the spike may open the card.
    param([Parameter(Mandatory)][string]$AsioModule)
    $problems = @()
    if (@(Get-Process -Name reaper -ErrorAction SilentlyContinue).Count -gt 0) { $problems += 'reaper.exe runs' }
    $holders = Get-GoldenAsioHolders -Module $AsioModule
    if ($holders.Count -gt 0) { $problems += "the ASIO module is held by $($holders -join ', ')" }
    if (@(Get-Process -Name asio_spike -ErrorAction SilentlyContinue).Count -gt 0) { $problems += 'a spike already runs' }
    return ,$problems
}

function New-SpikeArguments {
    param([Parameter(Mandatory)]$Request, [Parameter(Mandatory)][string]$Root)
    $status = Join-Path $Root 'status'
    $q = { param($s) '"' + $s + '"' }
    $a = @($Request.mode,
           '--driver', (& $q $Request.driver),
           '--report', (& $q (Join-Path $status ($Request.id + '.report.json'))),
           '--progress', (& $q (Join-Path $status ($Request.id + '.progress.json'))),
           '--stop-file', (& $q (Join-Path $Root 'queue\stop')))
    switch ($Request.mode) {
        'probe' { }
        'duplex' {
            if ($script:SpikeFrames -notcontains [int]$Request.frames) { throw "frames $($Request.frames) refused" }
            $a += @('--frames', [int]$Request.frames, '--seconds', [int]$Request.seconds, '--burn-us', [int]$Request.burn_us,
                    '--stress', [int]$Request.stress, '--panic-at', [long]$Request.panic_at)
        }
        'reopen' {
            if ($script:SpikeFrames -notcontains [int]$Request.frames) { throw "frames $($Request.frames) refused" }
            $a += @('--frames', [int]$Request.frames, '--cycles', [int]$Request.cycles)
        }
        default { throw "unknown mode $($Request.mode)" }
    }
    return ,([string[]]$a)
}

function Invoke-SpikeRun {
    # Runs in the console session (the Interactive task): one request, bounded.
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)]$Request)
    $bin = Join-Path $Root 'bin'
    $status = Join-Path $Root ("status\" + $Request.id + '.json')
    $stop = Join-Path $Root 'queue\stop'
    # "ide event" may have come between the request and this task: the stop file wins.
    if (Test-Path -LiteralPath $stop) { Write-GoldenStatus -Path $status -State 'refused' -Results @('the stop file exists (pre-empted before the start)'); return }
    [void](Test-SpikeSums -Bin $bin)
    $blockers = Get-SpikeBlockers -AsioModule $Request.module
    if ($blockers.Count -gt 0) { Write-GoldenStatus -Path $status -State 'refused' -Results $blockers; return }
    $spikeArgs = New-SpikeArguments -Request $Request -Root $Root
    $p = Start-Process -FilePath (Join-Path $bin 'asio_spike.exe') -ArgumentList $spikeArgs -PassThru -NoNewWindow `
        -RedirectStandardError (Join-Path $Root ("status\" + $Request.id + '.stderr.txt'))
    $null = $p.Handle   # keeps ExitCode readable after the exit (Windows PowerShell 5.1)
    $p.PriorityClass = [Diagnostics.ProcessPriorityClass]::High
    Write-GoldenStatus -Path $status -State 'running' -Results @([pscustomobject]@{ pid = $p.Id })
    $deadline = (Get-Date).AddSeconds([int]$Request.timeout)
    while (-not $p.WaitForExit(500)) {
        if ((Get-Date) -gt $deadline -and -not (Test-Path -LiteralPath $stop)) {
            New-Item -ItemType File -Force -Path $stop | Out-Null   # graceful: the spike polls the stop file
        }
    }
    Write-GoldenStatus -Path $status -State 'exited' -Results @([pscustomobject]@{ exit = $p.ExitCode })
}

function Register-SpikeTask {
    param([Parameter(Mandatory)][string]$Root)
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument ('-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "' + (Join-Path $Root 'bin\spike-task.ps1') + '"')
    # Over ssh USERDOMAIN is the workgroup, not the machine: take the token's own name.
    $principal = New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    Register-ScheduledTask -TaskPath $script:SpikeTaskPath -TaskName $script:SpikeTaskName -Action $action -Principal $principal -Settings $settings -Force | Out-Null
}

function Start-SpikeTask {
    Start-ScheduledTask -TaskPath $script:SpikeTaskPath -TaskName $script:SpikeTaskName
}

function Test-SpikeTaskBusy {
    # While the Interactive task runs (or is queued) a spike may be about to start.
    $t = Get-ScheduledTask -TaskPath $script:SpikeTaskPath -TaskName $script:SpikeTaskName -ErrorAction SilentlyContinue
    return [bool]($t -and (@('Running', 'Queued') -contains "$($t.State)"))
}

function Stop-SpikeGracefully {
    # Writes the stop file and waits until neither the spike nor its task runs
    # (a task that has not started the spike yet refuses on the stop file); never kills.
    param([Parameter(Mandatory)][string]$Root, [int]$Seconds = 60)
    New-Item -ItemType File -Force -Path (Join-Path $Root 'queue\stop') | Out-Null
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((@(Get-Process -Name asio_spike -ErrorAction SilentlyContinue).Count -gt 0) -or (Test-SpikeTaskBusy)) {
        if ((Get-Date) -gt $deadline) { return [pscustomobject]@{ gone = $false } }
        Start-Sleep -Milliseconds 500
    }
    [pscustomobject]@{ gone = $true }
}

function ConvertFrom-SpikeReaperLine {
    # One tab-separated line of REAPER's web interface: the fields after the verb.
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Text, [Parameter(Mandatory)][string]$Verb)
    foreach ($line in ($Text -split "`n")) {
        $f = $line.TrimEnd("`r") -split "`t"
        if ($f[0] -eq $Verb) { return ,@($f | Select-Object -Skip 1) }
    }
    return ,@()
}

function Get-SpikeExtState {
    param([Parameter(Mandatory)][string]$Http, [Parameter(Mandatory)][string]$SectionKey)
    $text = (Invoke-WebRequest -UseBasicParsing -Uri "$Http/_/GET/EXTSTATE/$SectionKey" -TimeoutSec 5).Content
    $f = ConvertFrom-SpikeReaperLine -Text $text -Verb 'EXTSTATE'
    if ($f.Count -ge 3) { return [string]$f[2] }
    return ''
}

function Invoke-SpikeBringBack {
    # "ide event" / end of window: REAPER back through our own start task, then
    # the handover checks (design note §5.4). The predecessor app kept running.
    # REAPER starts only with no spike or spike task running and the driver's
    # preferred buffer back at the recorded original (read here, not trusted).
    param([Parameter(Mandatory)][string]$Http, [Parameter(Mandatory)][string]$StartTaskPath, [Parameter(Mandatory)][string]$StartTask,
          [Parameter(Mandatory)][int]$NTrack, [Parameter(Mandatory)][string]$BridgeState, [Parameter(Mandatory)][string]$BridgeAction,
          [Parameter(Mandatory)][string]$Heartbeat, [Parameter(Mandatory)][string]$AsioModule, [Parameter(Mandatory)][string]$AppHttp,
          [Parameter(Mandatory)][string]$BufferKey, [Parameter(Mandatory)][string]$BufferName, [Parameter(Mandatory)][int]$Original,
          [string]$Raw = '')
    if (@(Get-Process -Name asio_spike -ErrorAction SilentlyContinue).Count -gt 0) { throw 'the spike still runs: REAPER may not start (I3)' }
    if (Test-SpikeTaskBusy) { throw 'the spike task still runs: REAPER may not start (I3)' }
    $pref = Get-SpikeBufferPref -Key $BufferKey -Name $BufferName
    if ($pref.value -ne $Original) { throw "the driver's preferred buffer is $($pref.value), not the original $Original (REAPER may not start)" }
    if ($Raw -and $pref.raw -ne $Raw) { throw "the driver's preferred buffer is '$($pref.raw)', not the original text '$Raw' (REAPER may not start)" }
    $holders = Get-GoldenAsioHolders -Module $AsioModule
    if (@($holders | Where-Object { $_ -notlike 'reaper.exe:*' }).Count -gt 0) { throw "the ASIO module is held by $($holders -join ', ')" }
    if (@(Get-Process reaper -ErrorAction SilentlyContinue).Count -eq 0) { Start-ScheduledTask -TaskPath $StartTaskPath -TaskName $StartTask }
    $deadline = (Get-Date).AddSeconds(120); $tracks = -1
    while ($tracks -ne $NTrack -and (Get-Date) -lt $deadline) {
        try { $f = ConvertFrom-SpikeReaperLine -Text (Invoke-WebRequest -UseBasicParsing -Uri "$Http/_/NTRACK" -TimeoutSec 5).Content -Verb 'NTRACK'; if ($f.Count -ge 1) { $tracks = [int]$f[0] } } catch { }
        if ($tracks -ne $NTrack) { Start-Sleep -Seconds 1 }
    }
    if ($tracks -ne $NTrack) { throw "REAPER did not load the project within 120 s (tracks $tracks, expected $NTrack)" }
    # The meter bridge: trigger it at most once, and only while its state is empty (a second trigger blocks REAPER with a dialog).
    $bridge = Get-SpikeExtState -Http $Http -SectionKey $BridgeState
    $triggered = $false
    if ($bridge -ne '1') {
        if ($bridge -ne '') { throw "meter bridge state is '$bridge': not triggered (only an empty state may be triggered)" }
        Invoke-WebRequest -UseBasicParsing -Uri "$Http/_/$BridgeAction" -TimeoutSec 5 | Out-Null
        $triggered = $true
    }
    $h1 = Get-SpikeExtState -Http $Http -SectionKey $Heartbeat
    Start-Sleep -Seconds 3
    $h2 = Get-SpikeExtState -Http $Http -SectionKey $Heartbeat
    if ($h1 -eq $h2) { throw "the meter heartbeat does not advance ('$h1')" }
    $holders = Get-GoldenAsioHolders -Module $AsioModule
    if (@($holders | Where-Object { $_ -like 'reaper.exe:*' }).Count -ne 1) { throw "REAPER does not hold the ASIO module: $($holders -join ', ')" }
    $app = Test-GoldenHttp -Uri $AppHttp
    if (-not ($app -gt 0 -and $app -lt 500)) { throw "the predecessor app does not answer (HTTP $app)" }
    [pscustomobject]@{ tracks = $tracks; bridge_triggered = $triggered; heartbeat = 'advancing'; asio = 'reaper'; app = $app }
}

Export-ModuleMember -Function *-Spike*
