#Requires -Version 5.1
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
# -Include is ignored next to -LiteralPath in Windows PowerShell 5.1: filter by extension.
foreach ($f in (Get-ChildItem -LiteralPath $here -Recurse -File | Where-Object { @('.ps1', '.psm1') -contains $_.Extension })) {
    $tokens = $null; $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($f.FullName, [ref]$tokens, [ref]$errors)
    if ($errors.Count -gt 0) { throw "parse errors in $($f.Name): $($errors[0].Message)" }
}
Import-Module (Join-Path $here 'GoldenPc.psm1') -Force
function Assert($cond, $what) { if (-not $cond) { throw "FAILED: $what" } ; Write-Host "ok  $what" }
function Throws([scriptblock]$b, $what) { $t = $false; try { & $b } catch { $t = $true }; Assert $t $what }

$base = Join-Path ([IO.Path]::GetTempPath()) ('golden-test-' + [guid]::NewGuid())
$a = Join-Path $base 'root a'; $b = Join-Path $base 'root-b'
New-Item -ItemType Directory -Force -Path (Join-Path $a 'sub dir'), $b | Out-Null
Set-Content -LiteralPath (Join-Path $a 'x.ini') -Value 'one'
Set-Content -LiteralPath (Join-Path $a 'sub dir\two words.txt') -Value 'two'
Set-Content -LiteralPath (Join-Path $b 'y.json') -Value '{}'
$roots = @{ 'a' = $a; 'b' = $b }
$backup = Join-Path $base 'backup'

$r = Invoke-GoldenBackup -Roots $roots -Dest $backup
Assert ($r.files -eq 3) 'backup-copies-every-file'
Assert ((Invoke-GoldenVerify -Backup $backup).identical) 'verify-identical-after-backup'

Set-Content -LiteralPath (Join-Path $a 'x.ini') -Value 'changed'
Remove-Item -LiteralPath (Join-Path $b 'y.json')
Set-Content -LiteralPath (Join-Path $a 'new.tmp') -Value 'extra'
$v = Invoke-GoldenVerify -Backup $backup
Assert (-not $v.identical) 'verify-detects-a-difference'
Assert ($v.files.changed.Count -eq 1 -and $v.files.missing.Count -eq 1 -and $v.files.extra.Count -eq 1) 'verify-detects-changed-missing-extra'
$v = Invoke-GoldenVerify -Backup $backup -Restore
Assert $v.identical 'restore-makes-identical'
Assert ($v.quarantined.Count -eq 1 -and (Test-Path -LiteralPath (Join-Path $backup 'quarantine\a\new.tmp'))) 'restore-quarantines-never-deletes'
Assert ((Get-Content -LiteralPath (Join-Path $a 'x.ini')) -eq 'one') 'restore-brings-back-content'
Throws { Invoke-GoldenBackup -Roots $roots -Dest $backup } 'backup-refuses-an-existing-destination'

# Volatile roots (a program that keeps running, e.g. the predecessor app): best effort, reported, never restored.
$c = Join-Path $base 'root-c'
New-Item -ItemType Directory -Force -Path $c | Out-Null
Set-Content -LiteralPath (Join-Path $c 'app.log') -Value 'log1'
Set-Content -LiteralPath (Join-Path $c 'held.db') -Value 'db'
$backup2 = Join-Path $base 'backup2'
$held = [IO.File]::Open((Join-Path $c 'held.db'), 'Open', 'ReadWrite', 'None')
try {
    Throws { Invoke-GoldenBackup -Roots @{ 'a' = $a; 'c' = $c } -Dest (Join-Path $base 'backup-strict') } 'backup-refuses-an-unreadable-file-in-a-strict-root'
    $r2 = Invoke-GoldenBackup -Roots @{ 'a' = $a; 'c' = $c } -Dest $backup2 -VolatileRoots @('c')
} finally { $held.Close() }
Assert ($r2.files -eq 4 -and $r2.volatile.unreadable -eq 1) 'backup-tolerates-a-held-file-in-a-volatile-root'
Throws { Invoke-GoldenBackup -Roots @{ 'a' = $a } -Dest (Join-Path $base 'backup3') -VolatileRoots @('nope') } 'backup-refuses-an-unknown-volatile-root'
Add-Content -LiteralPath (Join-Path $c 'app.log') -Value 'log2'
Set-Content -LiteralPath (Join-Path $c 'new.log') -Value 'x'
$v2 = Invoke-GoldenVerify -Backup $backup2 -Restore
Assert ($v2.identical -and $v2.volatile.changed -ge 1 -and $v2.volatile.extra -eq 1) 'volatile-changes-are-reported-not-failures'
Assert (@(Get-Content -LiteralPath (Join-Path $c 'app.log')).Count -eq 2 -and (Test-Path -LiteralPath (Join-Path $c 'new.log'))) 'volatile-root-is-never-restored'
Set-Content -LiteralPath (Join-Path $a 'x.ini') -Value 'changed again'
$v3 = Invoke-GoldenVerify -Backup $backup2 -Restore
Assert ($v3.identical -and $v3.restored.Count -eq 1 -and (Get-Content -LiteralPath (Join-Path $a 'x.ini')) -eq 'one') 'strict-root-next-to-a-volatile-one-is-restored'

$rpp = "<REAPER_PROJECT 0.1 `"7.65/win64`" 0 0`n  RENDER_FILE `"@@OUT@@\p`"`n  <JS utility/volume_pan `"`"`n  >`n  FILE `"@@JOB@@\stimuli\imp-dm-96000.wav`"`n>`n"
Test-GoldenRpp -Text $rpp; Assert $true 'rpp-allowlisted-passes'
Throws { Test-GoldenRpp -Text ($rpp.Replace('<JS utility/volume_pan ""', '<VST "VST3: Other" o.vst3 0 "" 1 ""')) } 'rpp-foreign-plugin-fails'
Throws { Test-GoldenRpp -Text ($rpp.Replace('@@JOB@@\stimuli\imp-dm-96000.wav', 'C:\Windows\x.wav')) } 'rpp-outside-media-fails'
Throws { Test-GoldenRpp -Text ($rpp.Replace('@@OUT@@\p', 'D:\elsewhere')) } 'rpp-outside-render-fails'

$bundle = Join-Path $base 'bundle'
New-Item -ItemType Directory -Force -Path (Join-Path $bundle 'projects'), (Join-Path $bundle 'stimuli') | Out-Null
[IO.File]::WriteAllText((Join-Path $bundle 'projects\p.rpp'), $rpp)
[IO.File]::WriteAllText((Join-Path $bundle 'stimuli\imp-dm-96000.wav'), 'RIFF-test')
$files = @(foreach ($rel in @('projects/p.rpp', 'stimuli/imp-dm-96000.wav')) { @{ path = $rel; sha256 = (Get-FileHash -LiteralPath (Join-Path $bundle ($rel -replace '/', '\')) -Algorithm SHA256).Hash.ToLowerInvariant() } })
[IO.File]::WriteAllText((Join-Path $bundle 'bundle.json'), (@{ schema = 1; projects = @(@{ file = 'projects/p.rpp'; id = 'p' }); files = $files } | ConvertTo-Json -Depth 5))
$job = Join-Path $base 'job one'
$staged = Invoke-GoldenStage -Bundle $bundle -Job $job
$text = [IO.File]::ReadAllText($staged[0])
Assert (-not $text.Contains('@@') -and $text.Contains("$job\stimuli\imp-dm-96000.wav") -and (Test-Path -LiteralPath (Join-Path $job 'out\p'))) 'stage-substitutes-tokens'
[IO.File]::WriteAllText((Join-Path $bundle 'stimuli\imp-dm-96000.wav'), 'RIFF-evil')
Throws { Invoke-GoldenStage -Bundle $bundle -Job (Join-Path $base 'job2') } 'stage-rejects-a-tampered-file'

$main = Join-Path $base 'main-resource'
New-Item -ItemType Directory -Force -Path (Join-Path $main 'Effects\utility'), (Join-Path $main 'Effects\loser') | Out-Null
Set-Content -LiteralPath (Join-Path $main 'Effects\utility\volume_pan') -Value 'desc:x'
Set-Content -LiteralPath (Join-Path $main 'Effects\loser\MGA_JSLimiterST') -Value 'desc:y'
Throws { New-GoldenResourceDir -Path (Join-Path $base 'res3') -MainResource $main -DummyMode 3 } 'resource-dir-refuses-asio'
$ini = New-GoldenResourceDir -Path (Join-Path $base 'res') -MainResource $main -DummyMode 4 -VstPath 'C:\Plugins\FX'
$iniText = Get-Content -LiteralPath $ini -Raw
Assert ($iniText.Contains('mode=4') -and -not ($iniText -match '(?i)asio') -and (Test-Path -LiteralPath (Join-Path $base 'res\Effects\loser\MGA_JSLimiterST'))) 'resource-dir-is-minimal-and-asio-free'
Assert ($iniText.Contains("vstpath64=C:\Plugins\FX`r`n")) 'resource-dir-pins-the-vst-path'

$st = Join-Path $base 's.json'
Write-GoldenStatus -Path $st -State 'done' -Results @([pscustomobject]@{ n = 1 })
Assert ((Get-Content -LiteralPath $st -Raw | ConvertFrom-Json).state -eq 'done') 'status-is-written-atomically'
$holders = Get-GoldenAsioHolders -Module 'no-such-module-xyz.dll'
Assert ($holders.Count -eq 0) 'asio-holders-empty-for-an-unloaded-module'
$holders = Get-GoldenAsioHolders -Module 'kernel32.dll'
Assert ($holders.Count -gt 1 -and $holders[0] -match '^[^:]+\.exe:\d+$') 'asio-holders-lists-image-and-pid'

# Registry keys are exported at backup and compared at verify (never written back).
$key = 'HKCU\Software\iemmixer-golden-selftest'
New-Item -Path "Registry::$key" -Force | Out-Null
New-ItemProperty -Path "Registry::$key" -Name 'v' -Value 'one' -Force | Out-Null
$backup4 = Join-Path $base 'backup4'
$r4 = Invoke-GoldenBackup -Roots @{ 'b' = $b } -Dest $backup4 -RegistryKeys @($key)
Assert ($r4.registry -eq 1 -and (Invoke-GoldenVerify -Backup $backup4).identical) 'registry-export-matches-itself'
Set-ItemProperty -Path "Registry::$key" -Name 'v' -Value 'two'
$v4 = Invoke-GoldenVerify -Backup $backup4
Assert (-not $v4.identical -and @($v4.registry.differences) -contains $key) 'registry-difference-is-reported'
Remove-Item -Path "Registry::$key" -Recurse -Force

# Over ssh USERDOMAIN is the workgroup, not the machine; the task must still register for this account.
$savedDomain = $env:USERDOMAIN
$env:USERDOMAIN = 'WORKGROUP'
try {
    Register-GoldenTask -Root $base -Name 'iemmixer-golden-selftest'
    $task = Get-ScheduledTask -TaskName 'iemmixer-golden-selftest'
    Assert ($task.Principal.LogonType -eq 'Interactive' -and $task.Settings.ExecutionTimeLimit -eq 'PT0S' -and $task.Settings.MultipleInstances -eq 'IgnoreNew') 'task-registers-interactive-unbounded-ignorenew-outside-a-domain'
} finally {
    $env:USERDOMAIN = $savedDomain
    Unregister-ScheduledTask -TaskName 'iemmixer-golden-selftest' -Confirm:$false -ErrorAction SilentlyContinue
}

Remove-Item -LiteralPath $base -Recurse -Force
Write-Host 'Test-GoldenPc: all passed'
