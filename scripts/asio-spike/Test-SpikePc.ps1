#Requires -Version 5.1
# Self-test of the S1a PC module on Windows PowerShell 5.1 (CI job asio-spike).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
foreach ($f in (Get-ChildItem -LiteralPath $here -File | Where-Object { @('.ps1', '.psm1') -contains $_.Extension })) {
    $tokens = $null; $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($f.FullName, [ref]$tokens, [ref]$errors)
    if ($errors.Count -gt 0) { throw "parse errors in $($f.Name): $($errors[0].Message)" }
}
# The bundle layout: SpikePc.psm1 next to GoldenPc.psm1.
$base = Join-Path ([IO.Path]::GetTempPath()) ('spike-test-' + [guid]::NewGuid())
$bin = Join-Path $base 'bin'
New-Item -ItemType Directory -Force -Path $bin, (Join-Path $base 'queue'), (Join-Path $base 'status') | Out-Null
Copy-Item -LiteralPath (Join-Path $here 'SpikePc.psm1'), (Join-Path $here '..\golden\GoldenPc.psm1') -Destination $bin
Import-Module (Join-Path $bin 'SpikePc.psm1') -Force
function Assert($cond, $what) { if (-not $cond) { throw "FAILED: $what" } ; Write-Host "ok  $what" }
function Throws([scriptblock]$b, $what) { $t = $false; try { & $b } catch { $t = $true }; Assert $t $what }
function ErrorOf([scriptblock]$b) { try { & $b; return '' } catch { return "$_" } }
$bringBack = @{ Http = 'http://127.0.0.1:9'; StartTaskPath = '\iemmixer-test\'; StartTask = 'iemmixer-no-such-task'; NTrack = 1
                BridgeState = 's/b'; BridgeAction = '0'; Heartbeat = 's/h'; AsioModule = 'iemmixer-no-such-module.dll'; AppHttp = 'http://127.0.0.1:9' }

# Driver preferred buffer: kind kept, read back, only 32/48/64 or the original.
$key = 'HKCU:\Software\iemmixer-spike-test-' + [guid]::NewGuid()
New-Item -Path $key -Force | Out-Null
try {
    New-ItemProperty -LiteralPath $key -Name 'Pref' -Value 64 -PropertyType DWord | Out-Null
    $p = Get-SpikeBufferPref -Key $key -Name 'Pref'
    Assert ($p.value -eq 64 -and $p.kind -eq 'DWord') 'buffer-pref-reads-value-and-kind'
    $r = Set-SpikeBufferPref -Key $key -Name 'Pref' -Value 32 -Original 64
    Assert ($r.before -eq 64 -and $r.after -eq 32 -and (Get-SpikeBufferPref -Key $key -Name 'Pref').value -eq 32) 'buffer-pref-set-reads-back'
    Throws { Set-SpikeBufferPref -Key $key -Name 'Pref' -Value 16 -Original 64 } 'buffer-pref-refuses-16'
    Throws { Set-SpikeBufferPref -Key $key -Name 'Pref' -Value 128 -Original 64 } 'buffer-pref-refuses-other-than-original'
    Assert ((Get-SpikeBufferPref -Key $key -Name 'Pref').value -eq 32) 'buffer-pref-refusal-changes-nothing'
    [void](Set-SpikeBufferPref -Key $key -Name 'Pref' -Value 128 -Original 128)
    Assert ((Get-SpikeBufferPref -Key $key -Name 'Pref').value -eq 128) 'buffer-pref-restores-any-recorded-original'
    New-ItemProperty -LiteralPath $key -Name 'Text' -Value '64' -PropertyType String | Out-Null
    [void](Set-SpikeBufferPref -Key $key -Name 'Text' -Value 48 -Original 64)
    $t = Get-SpikeBufferPref -Key $key -Name 'Text'
    Assert ($t.value -eq 48 -and $t.kind -eq 'String') 'buffer-pref-keeps-string-kind'
    New-ItemProperty -LiteralPath $key -Name 'Padded' -Value '064' -PropertyType String | Out-Null
    $p = Get-SpikeBufferPref -Key $key -Name 'Padded'
    Assert ($p.value -eq 64 -and $p.raw -eq '064') 'buffer-pref-reads-the-raw-text'
    [void](Set-SpikeBufferPref -Key $key -Name 'Padded' -Value 32 -Original 64)
    [void](Set-SpikeBufferPref -Key $key -Name 'Padded' -Value 64 -Original 64 -Raw '064')
    Assert ((Get-SpikeBufferPref -Key $key -Name 'Padded').raw -eq '064') 'buffer-pref-restores-the-raw-text'
    Throws { Set-SpikeBufferPref -Key $key -Name 'Padded' -Value 64 -Original 64 -Raw '48' } 'buffer-pref-refuses-raw-text-of-another-value'
    # REAPER comes back only at the recorded original (Pref holds 128 here).
    $e = ErrorOf { Invoke-SpikeBringBack @bringBack -BufferKey $key -BufferName 'Pref' -Original 64 }
    Assert ($e -like '*preferred buffer is 128*') "bring-back-refuses-a-changed-buffer ($e)"
    $e = ErrorOf { Invoke-SpikeBringBack @bringBack -BufferKey $key -BufferName 'Padded' -Original 64 -Raw '64' }
    Assert ($e -like "*preferred buffer is '064'*") "bring-back-refuses-changed-raw-text ($e)"
    $e = ErrorOf { Invoke-SpikeBringBack @bringBack -BufferKey $key -BufferName 'Pref' -Original 128 }
    Assert ($e -and $e -notlike '*preferred buffer*') "bring-back-passes-the-original-buffer ($e)"
    New-ItemProperty -LiteralPath $key -Name 'Blob' -Value ([byte[]](1, 2)) -PropertyType Binary | Out-Null
    Throws { Get-SpikeBufferPref -Key $key -Name 'Blob' } 'buffer-pref-refuses-binary'
} finally {
    Remove-Item -LiteralPath $key -Recurse -Force
}

# Bundle hashes.
$sums = Join-Path $base 'sums'
New-Item -ItemType Directory -Force -Path $sums | Out-Null
Set-Content -LiteralPath (Join-Path $sums 'a.exe') -Value 'binary'
$h = (Get-FileHash -LiteralPath (Join-Path $sums 'a.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
Set-Content -LiteralPath (Join-Path $sums 'SHA256SUMS') -Value "$h  a.exe"
$n = Test-SpikeSums -Bin $sums
Assert ($n.Count -eq 1 -and $n[0] -eq 'a.exe') 'sums-accept-a-matching-file'
Set-Content -LiteralPath (Join-Path $sums 'a.exe') -Value 'tampered'
Throws { Test-SpikeSums -Bin $sums } 'sums-detect-a-changed-file'
Set-Content -LiteralPath (Join-Path $sums 'SHA256SUMS') -Value "$h  ..\a.exe"
Throws { Test-SpikeSums -Bin $sums } 'sums-refuse-a-path'
Set-Content -LiteralPath (Join-Path $sums 'SHA256SUMS') -Value ''
Throws { Test-SpikeSums -Bin $sums } 'sums-refuse-an-empty-list'

# Spike arguments from a request.
$req = [pscustomobject]@{ id = 'spike-1'; mode = 'duplex'; driver = 'Some Card'; frames = 32; seconds = 600; burn_us = 100; stress = 4; panic_at = 0; cycles = 5
                          activity_channels = '101-110,121-124' }
$a = New-SpikeArguments -Request $req -Root 'C:\x y'
Assert ($a[0] -eq 'duplex' -and $a[2] -eq '"Some Card"' -and $a[4] -eq '"C:\x y\status\spike-1.report.json"' -and $a[8] -eq '"C:\x y\queue\stop"') 'arguments-quote-paths-and-driver'
Assert (($a -join ' ') -like '*--frames 32 --seconds 600 --burn-us 100 --stress 4 --panic-at 0 --activity-channels 101-110,121-124') 'arguments-duplex-options'
$req.mode = 'probe'
Assert ((New-SpikeArguments -Request $req -Root 'C:\r').Count -eq 9) 'arguments-probe-has-no-options'
$req.mode = 'reopen'
Assert (((New-SpikeArguments -Request $req -Root 'C:\r') -join ' ') -like '*--frames 32 --cycles 5 --activity-channels 101-110,121-124') 'arguments-reopen-options'
$req.activity_channels = 'all'
Assert (((New-SpikeArguments -Request $req -Root 'C:\r') -join ' ') -like '*--activity-channels all') 'arguments-watch-all-inputs-only-when-asked'
foreach ($bad in @('', '3; calc', '3 4', 'ALL', '"3"')) {
    $req.activity_channels = $bad
    Throws { New-SpikeArguments -Request $req -Root 'C:\r' } "arguments-refuse-activity-channels [$bad]"
}
$none = [pscustomobject]@{ id = 'spike-2'; mode = 'duplex'; driver = 'Some Card'; frames = 32; seconds = 600; burn_us = 0; stress = 0; panic_at = 0; cycles = 5 }
$e = ErrorOf { New-SpikeArguments -Request $none -Root 'C:\r' }
Assert ($e -like '*activity_channels*') "arguments-need-the-watched-inputs ($e)"
$req.activity_channels = '101-110,121-124'
$req.frames = 16
Throws { New-SpikeArguments -Request $req -Root 'C:\r' } 'arguments-refuse-frames-16'
$req.mode = 'record'
Throws { New-SpikeArguments -Request $req -Root 'C:\r' } 'arguments-refuse-an-unknown-mode'

# S1c: CPU Sets and hwlat. (duplex carries activity_channels since f154967; the
# CPU-set flags are appended in the duplex branch, before --activity-channels.
# hwlat is a clock-read loop with no audio stream, so it takes no watched inputs.)
$req = [pscustomobject]@{ id = 'spike-2'; mode = 'duplex'; driver = 'Some Card'; frames = 32; seconds = 28800; burn_us = 40; stress = 4; panic_at = 0; cycles = 5; activity_channels = '101-110,121-124'; audio_cpus = '14'; stress_cpus = '6-13' }
Assert (((New-SpikeArguments -Request $req -Root 'C:\r') -join ' ') -like '*--seconds 28800 *--audio-cpus 14 --stress-cpus 6-13 --activity-channels 101-110,121-124') 'arguments-cpu-sets'
$req.audio_cpus = '14;calc'
Throws { New-SpikeArguments -Request $req -Root 'C:\r' } 'arguments-refuse-a-bad-cpu-list'
$h = [pscustomobject]@{ id = 'spike-3'; mode = 'hwlat'; driver = 'Some Card'; frames = 0; seconds = 30; burn_us = 0; stress = 0; panic_at = 0; cycles = 1; cpu = 14; threshold_us = 10 }
$ha = New-SpikeArguments -Request $h -Root 'C:\r'
Assert ($ha[0] -eq 'hwlat' -and (($ha -join ' ') -like '*--cpu 14 --seconds 30 --threshold-us 10')) 'arguments-hwlat'
$h.cpu = 64
Throws { New-SpikeArguments -Request $h -Root 'C:\r' } 'arguments-refuse-cpu-64'
$h.cpu = 3; $h.threshold_us = 0
Throws { New-SpikeArguments -Request $h -Root 'C:\r' } 'arguments-refuse-threshold-0'

# REAPER web-interface lines.
$f = ConvertFrom-SpikeReaperLine -Text "NTRACK`t45`n" -Verb 'NTRACK'
Assert ($f.Count -eq 1 -and $f[0] -eq '45') 'reaper-line-ntrack'
$f = ConvertFrom-SpikeReaperLine -Text "TRACK`t1`n`r`nEXTSTATE`tsec`tkey`t1`r`n" -Verb 'EXTSTATE'
Assert ($f.Count -eq 3 -and $f[2] -eq '1') 'reaper-line-extstate-crlf'
Assert ((ConvertFrom-SpikeReaperLine -Text '' -Verb 'NTRACK').Count -eq 0) 'reaper-line-absent'

# Nothing blocks on a runner without REAPER or the card; a stop with no spike returns at once.
$b = Get-SpikeBlockers -AsioModule 'iemmixer-no-such-module.dll'
Assert ($b.Count -eq 0) 'blockers-none-without-reaper-or-card'
Assert ((Stop-SpikeGracefully -Root $base -Seconds 5).gone -and (Test-Path -LiteralPath (Join-Path $base 'queue\stop'))) 'stop-writes-the-stop-file'

Assert (-not (Test-SpikeTaskBusy)) 'task-not-busy-when-not-registered'

# A stop file already there (pre-empted before the task started the spike): refused before anything starts.
$pre = [pscustomobject]@{ id = 'spike-20260927T120000000'; kind = 'spike'; mode = 'probe'; driver = 'No Such Card'
                          module = 'iemmixer-no-such-module.dll'; frames = 0; seconds = 1; burn_us = 0; stress = 0; panic_at = 0; cycles = 1; timeout = 5 }
Invoke-SpikeRun -Root $base -Request $pre
$st = Get-Content -LiteralPath (Join-Path $base 'status\spike-20260927T120000000.json') -Raw | ConvertFrom-Json
Assert ($st.state -eq 'refused' -and "$($st.results)" -like '*stop file*') 'run-refuses-after-a-stop'

# spike-task.ps1: an unreadable request still leaves a status for the dev box.
$tb = Join-Path $base 'task'
New-Item -ItemType Directory -Force -Path (Join-Path $tb 'bin'), (Join-Path $tb 'queue'), (Join-Path $tb 'status') | Out-Null
Copy-Item -LiteralPath (Join-Path $here 'spike-task.ps1'), (Join-Path $bin 'SpikePc.psm1'), (Join-Path $bin 'GoldenPc.psm1') -Destination (Join-Path $tb 'bin')
function TaskStatus($json, $id) {
    [IO.File]::WriteAllText((Join-Path $tb 'queue\request.json'), $json)
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $tb 'bin\spike-task.ps1') | Out-Null
    $s = Join-Path $tb ("status\" + $id + '.json')
    $state = if (Test-Path -LiteralPath $s) { (Get-Content -LiteralPath $s -Raw | ConvertFrom-Json).state } else { 'none' }
    return "$LASTEXITCODE $state"
}
$r = TaskStatus '{ "id": "spike-20260927T120000001", "kind": ' 'spike-20260927T120000001'
Assert ($r -eq '1 failed') "task-reports-an-unreadable-request ($r)"
$r = TaskStatus '{ "id": "spike-20260927T120000002", "kind": "render" }' 'spike-20260927T120000002'
Assert ($r -eq '1 failed') "task-reports-an-unknown-kind ($r)"
$r = TaskStatus '{ "kind": "spike" }' 'request-unreadable'
Assert ($r -eq '1 failed') "task-reports-a-request-without-an-id ($r)"

Remove-Item -LiteralPath $base -Recurse -Force
Write-Host 'Test-SpikePc: all passed'
