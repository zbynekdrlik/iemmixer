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
$req = [pscustomobject]@{ id = 'spike-1'; mode = 'duplex'; driver = 'Some Card'; frames = 32; seconds = 600; burn_us = 100; stress = 4; panic_at = 0; cycles = 5 }
$a = New-SpikeArguments -Request $req -Root 'C:\x y'
Assert ($a[0] -eq 'duplex' -and $a[2] -eq '"Some Card"' -and $a[4] -eq '"C:\x y\status\spike-1.report.json"' -and $a[8] -eq '"C:\x y\queue\stop"') 'arguments-quote-paths-and-driver'
Assert (($a -join ' ') -like '*--frames 32 --seconds 600 --burn-us 100 --stress 4 --panic-at 0') 'arguments-duplex-options'
$req.mode = 'probe'
Assert ((New-SpikeArguments -Request $req -Root 'C:\r').Count -eq 9) 'arguments-probe-has-no-options'
$req.mode = 'reopen'
Assert (((New-SpikeArguments -Request $req -Root 'C:\r') -join ' ') -like '*--frames 32 --cycles 5') 'arguments-reopen-options'
$req.frames = 16
Throws { New-SpikeArguments -Request $req -Root 'C:\r' } 'arguments-refuse-frames-16'
$req.mode = 'record'
Throws { New-SpikeArguments -Request $req -Root 'C:\r' } 'arguments-refuse-an-unknown-mode'

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

Remove-Item -LiteralPath $base -Recurse -Force
Write-Host 'Test-SpikePc: all passed'
