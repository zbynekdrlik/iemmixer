#Requires -Version 5.1
# Self-test of the bootstrap's admin-only stage (#15) on Windows PowerShell 5.1
# (CI job windows, started by Test-IemPc.ps1, an ephemeral administrator
# runner): the script iempc.py's module_script composes (elevated_ps.py) runs
# here exactly as ssh sends it, against a temp elevated root. The upload is
# read once and checked, only the staged copy is imported, every folder and the
# staged file read back as Install-IemElevatedFolder makes them, and a junction
# or a foreign stage folder is refused with nothing written. The admin-only bin
# (#15): iempc_bin's install puts a checked iemmode.exe into <root>\bin through
# the stage, and the iemmode call iempc composes runs that copy only while it
# reads back admin-only, else PC_BIN's with a note.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
Import-Module (Join-Path $here 'IemPc.psm1') -Force
function Assert($cond, $what) { if (-not $cond) { throw "FAILED: $what" } ; Write-Host "ok  $what" }
function FileSha([string]$Path) { return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }

$id = [guid]::NewGuid().ToString('N').Substring(0, 8)
$base = Join-Path ([IO.Path]::GetTempPath()) ('iem-stage-' + $id)
$me = Resolve-IemUser
$compose = 'import sys; sys.path.insert(0, sys.argv[1]); import iempc; print(iempc.module_script(sys.argv[2], module=sys.argv[3], module_hex=sys.argv[4], elevated_root=sys.argv[5]))'
$probe = '[pscustomobject]@{ path = (Get-Module IemPc).Path }'

function Invoke-Staged([string]$Module, [string]$Hex, [string]$Root) {
    # The composed script, piped into a new Windows PowerShell as ssh pipes it.
    $script = @(& python -c $compose $here $probe $Module $Hex $Root)
    if ($LASTEXITCODE -ne 0) { throw "python exited $LASTEXITCODE" }
    $out = @($script | powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command -)
    if ($LASTEXITCODE -ne 0) { throw "powershell exited $LASTEXITCODE" }
    return (@($out | Where-Object { "$_".Trim() })[-1] | ConvertFrom-Json)
}

function Get-Result($Doc) { return ('ok={0} r={1} error={2}' -f (Get-IemProp $Doc 'ok'), (Get-IemProp (Get-IemProp $Doc 'r') 'path'), (Get-IemProp $Doc 'error')) }

$composeBin = 'import sys; sys.path.insert(0, sys.argv[1]); import iempc, iempc_bin; print(iempc_bin.install_script(iempc, sys.argv[2], sys.argv[3], elevated_root=sys.argv[4]))'
$composeRun = 'import sys; sys.path.insert(0, sys.argv[1]); import iempc, iempc_bin; print(iempc.native_script(sys.argv[2], [], then=iempc_bin.pick_for(iempc, sys.argv[3], elevated_root=sys.argv[4])))'

function Invoke-Composed([string[]]$PyArgs) {
    $script = @(& python -c @PyArgs)
    if ($LASTEXITCODE -ne 0) { throw "python exited $LASTEXITCODE" }
    $out = @($script | powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command -)
    if ($LASTEXITCODE -ne 0) { throw "powershell exited $LASTEXITCODE" }
    return (@($out | Where-Object { "$_".Trim() })[-1] | ConvertFrom-Json)
}

function Install-Bin([string]$Exe, [string]$Hex, [string]$Root) { return (Invoke-Composed @($composeBin, $here, $Exe, $Hex, $Root)) }
function Invoke-Bin([string]$PcBinExe, [string]$Want, [string]$Root) { return (Invoke-Composed @($composeRun, $here, $PcBinExe, $Want, $Root)) }
function Get-Run($Doc) { return ('exit={0} out={1} note={2} err={3}' -f (Get-IemProp $Doc 'exit'), (Get-IemProp $Doc 'out'), (Get-IemProp $Doc 'note'), (Get-IemProp $Doc 'err')) }

function New-FakeExe([string]$Path, [string]$Class, [string]$Says) {
    # A tiny console program in iemmode.exe's place: it prints which copy ran.
    $src = 'public static class ' + $Class + ' { public static int Main() { System.Console.WriteLine("' + $Says + '"); return 0; } }'
    Add-Type -TypeDefinition $src -OutputType ConsoleApplication -OutputAssembly $Path
}

try {
    # What the dev box uploads into the run folder of the user's root.
    $up = Join-Path $base 'upload'
    New-Item -ItemType Directory -Force -Path $up | Out-Null
    $src = Join-Path $up 'IemPc.psm1'
    Copy-Item -LiteralPath (Join-Path $here 'IemPc.psm1') -Destination $src
    $hex = FileSha $src

    # ---- the happy path: a new elevated root, the stage, the staged copy imported ----
    $er = Join-Path $base 'er-new'
    $stage = Join-Path $er 'bootstrap-stage'
    $staged = Join-Path $stage 'IemPc.psm1'
    $doc = Invoke-Staged $src $hex $er
    Assert ((Get-IemProp $doc 'ok') -eq $true -and $doc.r.path -eq $staged) "stage-imports-the-staged-copy-never-the-upload ($(Get-Result $doc))"
    foreach ($p in @($er, $stage, $staged)) {
        $bad = Test-IemElevatedItem -Path $p -UserSid $me.sid
        Assert ($bad.Count -eq 0) "stage-reads-back-as-an-elevated-item [$p] ($($bad -join '; '))"
    }
    Assert ((FileSha $staged) -ceq $hex) 'stage-holds-the-checked-bytes'
    # A stage that already holds exactly the checked bytes is read, never rewritten: sessions that
    # run at the same time (a window's poll and a preempt) never write under another's import.
    $written = (Get-Item -LiteralPath $staged).LastWriteTimeUtc
    Start-Sleep -Milliseconds 50
    $doc = Invoke-Staged $src $hex $er
    Assert ((Get-IemProp $doc 'ok') -eq $true -and $doc.r.path -eq $staged) "stage-again-imports-the-staged-copy ($(Get-Result $doc))"
    Assert ((Get-Item -LiteralPath $staged).LastWriteTimeUtc -eq $written) 'stage-again-keeps-the-identical-copy'

    # ---- an upload that is not the attested one: refused, nothing staged or imported ----
    Add-Content -LiteralPath $src -Value '# changed after the sums'
    $doc = Invoke-Staged $src $hex $er
    Assert ((Get-IemProp $doc 'ok') -eq $false -and "$(Get-IemProp $doc 'error')" -like '*sha256 mismatch*') "stage-refuses-an-upload-with-another-hash ($(Get-Result $doc))"
    Assert ((FileSha $staged) -ceq $hex) 'stage-keeps-the-copy-it-had-after-a-refusal'
    Copy-Item -LiteralPath (Join-Path $here 'IemPc.psm1') -Destination $src -Force

    # ---- a junction in place of the stage: refused, nothing written through it ----
    $outside = Join-Path $base 'outside'
    New-Item -ItemType Directory -Force -Path $outside | Out-Null
    $jr = Join-Path $base 'er-junction'
    Install-IemElevatedFolder -Path $jr -UserSid $me.sid
    New-Item -ItemType Junction -Path (Join-Path $jr 'bootstrap-stage') -Value $outside | Out-Null
    $doc = Invoke-Staged $src $hex $jr
    Assert ((Get-IemProp $doc 'ok') -eq $false -and "$(Get-IemProp $doc 'error')" -like '*junction or a link*' -and
            @(Get-ChildItem -LiteralPath $outside -Force).Count -eq 0) "stage-refuses-a-junction-and-writes-nothing ($(Get-Result $doc))"

    # ---- a stage folder someone else made (owned by the user): refused ----
    $fr = Join-Path $base 'er-foreign'
    Install-IemElevatedFolder -Path $fr -UserSid $me.sid
    $foreign = Join-Path $fr 'bootstrap-stage'
    New-Item -ItemType Directory -Force -Path $foreign | Out-Null
    $ds = New-Object System.Security.AccessControl.DirectorySecurity
    $ds.SetOwner((New-Object System.Security.Principal.SecurityIdentifier $me.sid))
    [IO.Directory]::SetAccessControl($foreign, $ds)
    $doc = Invoke-Staged $src $hex $fr
    Assert ((Get-IemProp $doc 'ok') -eq $false -and "$(Get-IemProp $doc 'error')" -like '*is owned by*refused*' -and
            -not (Test-Path -LiteralPath (Join-Path $foreign 'IemPc.psm1'))) "stage-refuses-a-folder-someone-else-made ($(Get-Result $doc))"

    # ---- the admin-only bin (#15): the attested iemmode.exe goes in through the stage, and an
    # iemmode call runs that copy only while it reads back admin-only, else PC_BIN's with a note ----
    $exe = Join-Path $up 'iemmode.exe'
    New-FakeExe -Path $exe -Class 'IemStageBinCopy' -Says 'bin-copy'
    $exeHex = FileSha $exe
    $userBin = Join-Path $base 'userbin'
    New-Item -ItemType Directory -Force -Path $userBin | Out-Null
    $userExe = Join-Path $userBin 'iemmode.exe'
    New-FakeExe -Path $userExe -Class 'IemStageUserBin' -Says 'user-bin'
    $br = Join-Path $base 'er-bin'
    $copy = Join-Path $br 'bin\iemmode.exe'
    $doc = Invoke-Bin $userExe $exeHex $br
    Assert ((Get-IemProp $doc 'exit') -eq 0 -and "$(Get-IemProp $doc 'out')".Trim() -eq 'user-bin' -and "$(Get-IemProp $doc 'note')") "bin-none-yet-runs-pc-bin-with-a-note ($(Get-Run $doc))"
    $doc = Install-Bin $exe $exeHex $br
    Assert ((Get-IemProp $doc 'ok') -eq $true -and (Get-IemProp $doc 'r') -eq $exeHex) "bin-install-answers-the-read-back-sha256 ($(Get-IemProp $doc 'error'))"
    foreach ($p in @($br, (Join-Path $br 'bin'), $copy)) {
        $bad = Test-IemElevatedItem -Path $p -UserSid $me.sid
        Assert ($bad.Count -eq 0) "bin-reads-back-as-an-elevated-item [$p] ($($bad -join '; '))"
    }
    Assert ((FileSha $copy) -ceq $exeHex) 'bin-holds-the-checked-bytes'
    $doc = Invoke-Bin $userExe $exeHex $br
    Assert ((Get-IemProp $doc 'exit') -eq 0 -and "$(Get-IemProp $doc 'out')".Trim() -eq 'bin-copy' -and -not (Get-IemProp $doc 'note')) "bin-iemmode-runs-the-admin-only-copy ($(Get-Run $doc))"
    # A copy that is not the build the dev box installed (a later activation by another path) is never run.
    $doc = Invoke-Bin $userExe (FileSha $userExe) $br
    Assert ("$(Get-IemProp $doc 'out')".Trim() -eq 'user-bin' -and "$(Get-IemProp $doc 'note')" -like '*not the build*') "bin-a-copy-of-another-build-is-never-run ($(Get-Run $doc))"
    $written = (Get-Item -LiteralPath $copy).LastWriteTimeUtc
    Start-Sleep -Milliseconds 50
    $doc = Install-Bin $exe $exeHex $br
    Assert ((Get-IemProp $doc 'ok') -eq $true -and (Get-Item -LiteralPath $copy).LastWriteTimeUtc -eq $written) "bin-install-again-keeps-the-identical-copy ($(Get-IemProp $doc 'error'))"
    $other = Join-Path $up 'iemmode-other.exe'
    [IO.File]::WriteAllBytes($other, [byte[]](@([IO.File]::ReadAllBytes($exe)) + @(0)))
    $doc = Install-Bin $other $exeHex $br
    Assert ((Get-IemProp $doc 'ok') -eq $false -and "$(Get-IemProp $doc 'error')" -like '*sha256 mismatch*' -and (FileSha $copy) -ceq $exeHex) "bin-refuses-an-upload-with-another-hash-and-keeps-its-copy ($(Get-IemProp $doc 'error'))"
    $acl = Get-Acl -LiteralPath $copy
    $acl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule((New-Object System.Security.Principal.SecurityIdentifier $me.sid), 'Modify', 'Allow')))
    Set-Acl -LiteralPath $copy -AclObject $acl
    $doc = Invoke-Bin $userExe $exeHex $br
    Assert ("$(Get-IemProp $doc 'out')".Trim() -eq 'user-bin' -and "$(Get-IemProp $doc 'note')" -like '*may be changed by*') "bin-a-copy-the-user-may-change-is-never-run ($(Get-Run $doc))"
} finally {
    Remove-Item -LiteralPath $base -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host 'Test-IemStage: all passed'
