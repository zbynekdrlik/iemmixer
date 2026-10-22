#Requires -Version 5.1
# Self-test of iempc roster-install's PowerShell bodies (#58, the review of Part
# D on #62) on Windows PowerShell 5.1 (CI job windows, started by
# Test-IemPc.ps1): iempc_roster.py composes each body and iempc_core's
# module_script wraps it exactly as ssh sends it; each runs here in a new
# Windows PowerShell against a temp folder standing for the server's config
# folder. The pc.toml and status reads give the text or $null; the install
# keeps the previous roster.toml and its sha256, puts the upload in place by
# a move (none there) or [IO.File]::Replace (one there), checked by sha256,
# and refuses an upload of other bytes with nothing changed; the restore puts
# the kept file back, or removes roster.toml when there was none, refuses
# while roster.toml holds anything but the upload, and keeps a roster the
# server applied meanwhile (the applied roster or the status names it).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
Import-Module (Join-Path $here 'IemPc.psm1') -Force
function Assert($cond, $what) { if (-not $cond) { throw "FAILED: $what" } ; Write-Host "ok  $what" }
function FileSha([string]$Path) { return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }

# The bodies as iempc composes them (python -c, no quote inside: Windows PowerShell 5.1 drops the double
# quotes of a native command's argument; chr 45 is the dash that stands for "no previous file").
$pre = 'import sys; sys.path.insert(0, sys.argv[1]); import iempc, iempc_roster as r; '
$composeToml = $pre + 'print(iempc.module_script(r.toml_body(iempc, sys.argv[2])))'
$composeStatus = $pre + 'print(iempc.module_script(r.status_body(iempc, sys.argv[2])))'
$composeInstall = $pre + 'print(iempc.module_script(r.install_body(iempc, sys.argv[2], sys.argv[3], sys.argv[4])))'
$composeRestore = $pre + 'print(iempc.module_script(r.restore_body(iempc, sys.argv[2], sys.argv[3], None if sys.argv[4] == chr(45) else sys.argv[4])))'

function Invoke-Composed([string[]]$PyArgs) {
    # The composed script, piped into a new Windows PowerShell as ssh pipes it; its last line, parsed.
    $script = @(& python -c @PyArgs)
    if ($LASTEXITCODE -ne 0) { throw "python exited $LASTEXITCODE" }
    $out = @($script | powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command -)
    if ($LASTEXITCODE -ne 0) { throw "powershell exited $LASTEXITCODE" }
    return (@($out | Where-Object { "$_".Trim() })[-1] | ConvertFrom-Json)
}

function Get-Answer($Doc) { return ('ok={0} error={1}' -f (Get-IemProp $Doc 'ok'), (Get-IemProp $Doc 'error')) }

function Write-Text([string]$Path, [string]$Text) { [IO.File]::WriteAllBytes($Path, [Text.Encoding]::UTF8.GetBytes($Text)) }

$id = [guid]::NewGuid().ToString('N').Substring(0, 8)
$base = Join-Path ([IO.Path]::GetTempPath()) ('iem-roster-' + $id)
try {
    $root = Join-Path $base 'root'
    $cfg = Join-Path $base 'site'
    $incoming = Join-Path $root 'incoming'
    foreach ($d in @((Join-Path $root 'guard'), $incoming, (Join-Path $cfg 'lineups'))) { New-Item -ItemType Directory -Force -Path $d | Out-Null }
    $dst = Join-Path $cfg 'roster.toml'
    $prev = Join-Path $cfg 'roster.toml.prev'
    $new = Join-Path $cfg 'roster.toml.new'
    $statusFile = Join-Path $cfg 'roster.status.json'
    $applied = Join-Path $cfg 'lineups\applied-roster.toml'

    # ---- the reads: the text, or $null without the file ----
    $doc = Invoke-Composed @($composeToml, $here, $root)
    Assert ((Get-IemProp $doc 'ok') -eq $true -and $null -eq (Get-IemProp $doc 'r')) "toml-read-without-pc-toml-is-null ($(Get-Answer $doc))"
    Write-Text (Join-Path $root 'guard\pc.toml') "server_config = 'X:\site\iemmixer.toml'`n"
    $doc = Invoke-Composed @($composeToml, $here, $root)
    Assert ("$(Get-IemProp $doc 'r')" -like "server_config = *iemmixer.toml*") "toml-read-gives-the-text ($(Get-Answer $doc))"
    $doc = Invoke-Composed @($composeStatus, $here, $cfg)
    Assert ((Get-IemProp $doc 'ok') -eq $true -and $null -eq (Get-IemProp $doc 'r')) "status-read-without-a-status-is-null ($(Get-Answer $doc))"
    $statusText = '{"sha256":"' + ('0f' * 32) + '","ok":true,"applied":true}'
    Write-Text $statusFile $statusText
    $doc = Invoke-Composed @($composeStatus, $here, $cfg)
    Assert ("$(Get-IemProp $doc 'r')" -ceq $statusText) "status-read-gives-the-text ($(Get-Answer $doc))"

    # ---- the first install: no roster.toml, so a move and no previous file ----
    $upA = Join-Path $incoming 'roster-a.toml'
    Write-Text $upA "# roster a`n"
    $shaA = FileSha $upA
    $doc = Invoke-Composed @($composeInstall, $here, $upA, $cfg, $shaA)
    $r = Get-IemProp $doc 'r'
    Assert ((Get-IemProp $doc 'ok') -eq $true -and (Get-IemProp $r 'installed') -eq $true) "install-first-answers-installed ($(Get-Answer $doc))"
    Assert ($null -eq (Get-IemProp $r 'prev') -and $null -eq (Get-IemProp $r 'applied')) 'install-first-names-no-previous-file-and-no-applied-roster'
    Assert ("$(Get-IemProp $r 'before')" -ceq $statusText) 'install-reads-the-status-before-the-move'
    Assert ((FileSha $dst) -ceq $shaA -and -not (Test-Path -LiteralPath $new) -and -not (Test-Path -LiteralPath $prev)) 'install-first-moves-the-upload-in-place'

    # ---- the next install: roster.toml there, so it is kept and replaced ----
    Write-Text $applied "# roster a`n"
    $upB = Join-Path $incoming 'roster-b.toml'
    Write-Text $upB "# roster b`n"
    $shaB = FileSha $upB
    $doc = Invoke-Composed @($composeInstall, $here, $upB, $cfg, $shaB)
    $r = Get-IemProp $doc 'r'
    Assert ((Get-IemProp $doc 'ok') -eq $true -and (Get-IemProp $r 'prev') -ceq $shaA) "install-again-names-the-previous-file ($(Get-Answer $doc))"
    Assert ((Get-IemProp $r 'applied') -ceq $shaA) 'install-names-the-applied-roster-by-its-sha256'
    Assert ((FileSha $dst) -ceq $shaB -and (FileSha $prev) -ceq $shaA -and -not (Test-Path -LiteralPath $new)) 'install-again-replaces-roster-toml-and-keeps-the-previous-one'

    # ---- an upload of other bytes than its sha256 changes nothing ----
    $doc = Invoke-Composed @($composeInstall, $here, $upA, $cfg, $shaB)
    Assert ((Get-IemProp $doc 'ok') -eq $false -and "$(Get-IemProp $doc 'error')" -like '*sha256 mismatch*') "install-refuses-another-upload ($(Get-Answer $doc))"
    Assert ((FileSha $dst) -ceq $shaB -and (FileSha $prev) -ceq $shaA) 'install-refused-changes-nothing'

    # ---- the restore: only while roster.toml holds the upload ----
    $doc = Invoke-Composed @($composeRestore, $here, $cfg, $shaA, $shaA)
    Assert ((Get-IemProp $doc 'ok') -eq $false -and "$(Get-IemProp $doc 'error')" -like '*sha256 mismatch*') "restore-refuses-a-roster-toml-of-other-bytes ($(Get-Answer $doc))"
    Assert ((FileSha $dst) -ceq $shaB) 'restore-refused-changes-nothing'
    $doc = Invoke-Composed @($composeRestore, $here, $cfg, $shaB, $shaA)
    Assert ((Get-IemProp $doc 'ok') -eq $true -and (Get-IemProp $doc 'r') -ceq 'restored') "restore-puts-the-previous-file-back ($(Get-Answer $doc))"
    Assert ((FileSha $dst) -ceq $shaA -and -not (Test-Path -LiteralPath $new)) 'restore-roster-toml-holds-the-previous-bytes'
    # ---- a roster the server applied meanwhile stays: by the applied roster, or by the status ----
    $doc = Invoke-Composed @($composeRestore, $here, $cfg, $shaA, '-')
    Assert ((Get-IemProp $doc 'ok') -eq $true -and (Get-IemProp (Get-IemProp $doc 'r') 'late') -eq $true) "restore-skips-a-roster-the-server-applied ($(Get-Answer $doc))"
    Assert ((FileSha $dst) -ceq $shaA) 'restore-skipped-keeps-roster-toml'
    Remove-Item -LiteralPath $applied
    Write-Text $statusFile ('{"sha256":"' + $shaA + '","ok":true,"applied":true}')
    $doc = Invoke-Composed @($composeRestore, $here, $cfg, $shaA, '-')
    Assert ((Get-IemProp $doc 'ok') -eq $true -and (Get-IemProp (Get-IemProp $doc 'r') 'late') -eq $true) "restore-skips-a-roster-the-status-names-applied ($(Get-Answer $doc))"
    Assert ((FileSha $dst) -ceq $shaA) 'restore-skipped-by-the-status-keeps-roster-toml'
    # ---- refused: no previous file, so roster.toml goes ----
    Write-Text $statusFile ('{"sha256":"' + $shaA + '","ok":false,"applied":false}')
    $doc = Invoke-Composed @($composeRestore, $here, $cfg, $shaA, '-')
    Assert ((Get-IemProp $doc 'ok') -eq $true -and (Get-IemProp $doc 'r') -ceq 'restored') "restore-without-a-previous-file ($(Get-Answer $doc))"
    Assert (-not (Test-Path -LiteralPath $dst)) 'restore-without-a-previous-file-removes-roster-toml'
} finally {
    Remove-Item -LiteralPath $base -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host 'Test-IemRoster: all passed'
