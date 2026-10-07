#Requires -Version 5.1
# Self-test of Install-IemTuning (IemPc.psm1, #15) on Windows PowerShell 5.1 (CI
# job windows, started by Test-IemPc.ps1, an ephemeral administrator runner):
# S1c's tuning modules and a synthetic profile into a temp elevated root. The
# real IemTuning.psm1 checks the profiles (profile_cases.json's layouts), every
# refusal writes nothing, and nothing is written through a junction.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$tuningSrc = Join-Path (Split-Path -Parent $here) 'pc-tuning'
Import-Module (Join-Path $here 'IemPc.psm1') -Force
function Assert($cond, $what) { if (-not $cond) { throw "FAILED: $what" } ; Write-Host "ok  $what" }
function ErrorOf([scriptblock]$b) { try { & $b; return '' } catch { return "$_" } }
function FileSha([string]$Path) { return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }

$id = [guid]::NewGuid().ToString('N').Substring(0, 8)
$base = Join-Path ([IO.Path]::GetTempPath()) ('iem-tuning-install-' + $id)
$me = Resolve-IemUser
$cases = Get-Content -LiteralPath (Join-Path $tuningSrc 'profile_cases.json') -Raw | ConvertFrom-Json
$goodLayout = [ordered]@{ housekeeping = @(0); nic = @(1); card = @(2); audio = @(3) }
$names = @('IemTuning.psm1', 'IemMeasure.psm1', 'profile.json')

function New-Source([string]$Name, $Layout, [string[]]$Drop = @(), [switch]$NoProfile) {
    # What the dev box uploads: the real modules and a synthetic profile
    # (processor numbers and test names only, no site value).
    $d = Join-Path $base $Name
    New-Item -ItemType Directory -Force -Path $d | Out-Null
    foreach ($m in 'IemTuning.psm1', 'IemMeasure.psm1') { Copy-Item -LiteralPath (Join-Path $tuningSrc $m) -Destination $d }
    if ($NoProfile) { return $d }
    $p = [ordered]@{
        version = 1; journal = (Join-Path $d 'journal.json'); registry_root = 'HKCU:\Software\iemmixer-tuning-install-test'
        layout = $Layout
        plan = [ordered]@{ guid = '00000000-0000-0000-0000-00000000000a'; source = '00000000-0000-0000-0000-00000000000b' }
        governor = 'gov'; placement = @(); services_disable = @(); services_mode = @()
        updates = [ordered]@{ services = @(); tasks = @() }; maintenance = [ordered]@{ off = $true; tasks = @() }
        defender = [ordered]@{ paths = @(); processes = @() }; devices = @()
        nic = [ordered]@{ adapter = 'a'; properties = [ordered]@{}; rss = [ordered]@{ base = 1; max = 1 }; pnp_capabilities = 24 }
        fingerprint = [ordered]@{ files = @(); keys = @() }
    }
    foreach ($k in $Drop) { $p.Remove($k) }
    [IO.File]::WriteAllText((Join-Path $d 'profile.json'), ($p | ConvertTo-Json -Depth 6))
    return $d
}

function New-ElevatedRoot([string]$Name) {
    # An elevated root as Register-IemTasks makes it: owned by Administrators, protected, the user reads.
    $r = Join-Path $base $Name
    Install-IemElevatedFolder -Path $r -UserSid $me.sid
    return $r
}

function Get-SourceHashes([string]$Src) {
    $h = @{ TuningSha256 = (FileSha (Join-Path $Src 'IemTuning.psm1')); MeasureSha256 = (FileSha (Join-Path $Src 'IemMeasure.psm1')) }
    if (Test-Path -LiteralPath (Join-Path $Src 'profile.json')) { $h.ProfileSha256 = (FileSha (Join-Path $Src 'profile.json')) }
    return $h
}

function Get-Installed([string]$Root) {
    # The tuning folder's files and their hashes, '' for one that is absent.
    $t = Join-Path $Root 'tuning'
    return (@($names | ForEach-Object { $p = Join-Path $t $_; if (Test-Path -LiteralPath $p -PathType Leaf) { "${_}=$(FileSha $p)" } else { "${_}=" } }) -join ';')
}

try {
    # ---- the happy path: written fresh, admin-owned, read back ----
    $er = New-ElevatedRoot 'er-ok'
    $src = New-Source 'src-ok' $goodLayout
    $h = Get-SourceHashes $src
    $r = Install-IemTuning -Root $er -SourceDir $src @h
    Assert ((@($r.PSObject.Properties.Name) -join ',') -ceq 'tuning,measure,profile') "tuning-install-returns-the-three-hashes-only ($(@($r.PSObject.Properties.Name) -join ','))"
    Assert ($r.tuning -ceq $h.TuningSha256 -and $r.measure -ceq $h.MeasureSha256 -and $r.profile -ceq $h.ProfileSha256) 'tuning-install-returns-the-hashes-it-read-back'
    $tuning = Join-Path $er 'tuning'
    $tb = Test-IemElevatedItem -Path $tuning -UserSid $me.sid
    Assert ($tb.Count -eq 0) "tuning-install-makes-an-admin-only-tuning-folder ($($tb -join '; '))"
    foreach ($n in $names) {
        $p = Join-Path $tuning $n
        Assert ((FileSha $p) -ceq (FileSha (Join-Path $src $n))) "tuning-install-writes-the-uploaded-bytes [$n]"
        $fb = Test-IemElevatedItem -Path $p -UserSid $me.sid
        $fa = Get-Acl -LiteralPath $p
        $fr = @($fa.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
        Assert ($fb.Count -eq 0 -and $fa.GetOwner([System.Security.Principal.SecurityIdentifier]).Value -eq 'S-1-5-32-544' -and
                @($fr | Where-Object { -not $_.IsInherited }).Count -eq 0) "tuning-install-file-admin-owned-rules-inherited [$n] ($($fb -join '; '))"
    }
    $installed = Get-Installed $er
    # Again with the same files: the same result (written fresh each time).
    $r2 = Install-IemTuning -Root $er -SourceDir $src @h
    Assert ($r2.profile -ceq $h.ProfileSha256 -and (Get-Installed $er) -ceq $installed) 'tuning-install-again-is-the-same'

    # ---- a wrong hash writes nothing ----
    $src2 = New-Source 'src-changed' $goodLayout
    Add-Content -LiteralPath (Join-Path $src2 'IemMeasure.psm1') -Value '# changed after the sums'
    $h2 = Get-SourceHashes $src2
    $h2.MeasureSha256 = $h.MeasureSha256
    $e = ErrorOf { Install-IemTuning -Root $er -SourceDir $src2 @h2 }
    Assert ($e -like '*IemMeasure.psm1: sha256*refused*' -and (Get-Installed $er) -ceq $installed) "tuning-install-a-wrong-hash-changes-nothing ($e)"
    $empty = New-ElevatedRoot 'er-empty'
    $e = ErrorOf { Install-IemTuning -Root $empty -SourceDir $src2 @h2 }
    Assert ($e -like '*sha256*refused*' -and -not (Test-Path -LiteralPath (Join-Path $empty 'tuning'))) "tuning-install-a-wrong-hash-makes-no-folder ($e)"
    $bad = Get-SourceHashes $src
    $bad.TuningSha256 = $bad.TuningSha256.ToUpperInvariant()
    $e = ErrorOf { Install-IemTuning -Root $empty -SourceDir $src @bad }
    Assert ($e -like '*not 64 lowercase hex*' -and -not (Test-Path -LiteralPath (Join-Path $empty 'tuning'))) "tuning-install-refuses-a-hash-that-is-not-lowercase-hex ($e)"

    # ---- a profile IemTuning's own loader refuses writes nothing ----
    $badLayouts = @('two-roles-share-a-processor', 'a-string', 'processor-64', 'a-null-role', 'a-scalar-role', 'a-layout-that-is-not-an-object')
    foreach ($c in @($cases.layouts | Where-Object { $badLayouts -contains $_.name })) {
        Assert (-not $c.ok) "tuning-install-case-is-a-refusal [$($c.name)]"
        $s = New-Source ('src-' + $c.name) $c.layout
        $hs = Get-SourceHashes $s
        $e = ErrorOf { Install-IemTuning -Root $empty -SourceDir $s @hs }
        Assert ($e -like '*layout*' -and -not (Test-Path -LiteralPath (Join-Path $empty 'tuning'))) "tuning-install-a-refused-layout-writes-nothing [$($c.name)] ($e)"
        $e = ErrorOf { Install-IemTuning -Root $er -SourceDir $s @hs }
        Assert ($e -like '*layout*' -and (Get-Installed $er) -ceq $installed) "tuning-install-a-refused-layout-keeps-the-installed-files [$($c.name)]"
    }
    $s = New-Source 'src-no-nic' $goodLayout -Drop @('nic')
    $hs = Get-SourceHashes $s
    $e = ErrorOf { Install-IemTuning -Root $er -SourceDir $s @hs }
    Assert ($e -like "*missing 'nic'*" -and (Get-Installed $er) -ceq $installed) "tuning-install-a-profile-without-a-key-writes-nothing ($e)"
    $e = ErrorOf { Install-IemTuning -Root $er -SourceDir $src -TuningSha256 $h.TuningSha256 -MeasureSha256 $h.MeasureSha256 }
    Assert ($e -like '*-ProfileSha256*-KeepProfile*' -and (Get-Installed $er) -ceq $installed) "tuning-install-needs-a-profile-or-keep-profile ($e)"
    $e = ErrorOf { Install-IemTuning -Root $er -SourceDir $src @h -KeepProfile }
    Assert ($e -like '*-ProfileSha256*-KeepProfile*' -and (Get-Installed $er) -ceq $installed) "tuning-install-refuses-a-profile-and-keep-profile ($e)"

    # ---- -KeepProfile (the activation's refresh): the installed profile stays ----
    $src3 = New-Source 'src-modules' $goodLayout -NoProfile
    Add-Content -LiteralPath (Join-Path $src3 'IemMeasure.psm1') -Value '# the next bundle'
    $h3 = Get-SourceHashes $src3
    $r3 = Install-IemTuning -Root $er -SourceDir $src3 @h3 -KeepProfile
    Assert ($r3.measure -ceq $h3.MeasureSha256 -and $r3.measure -cne $h.MeasureSha256 -and $r3.profile -ceq $h.ProfileSha256) 'tuning-install-keep-profile-replaces-the-modules'
    Assert ((FileSha (Join-Path $tuning 'profile.json')) -ceq $h.ProfileSha256) 'tuning-install-keep-profile-leaves-the-profile'
    $e = ErrorOf { Install-IemTuning -Root $empty -SourceDir $src3 @h3 -KeepProfile }
    Assert ($e -like '*installed profile*' -and -not (Test-Path -LiteralPath (Join-Path $empty 'tuning'))) "tuning-install-keep-profile-without-one-writes-nothing ($e)"
    $installed = Get-Installed $er
    $pf = Join-Path $tuning 'profile.json'
    $fs = [IO.File]::GetAccessControl($pf)
    $fs.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule((New-Object System.Security.Principal.SecurityIdentifier $me.sid), 'Modify', 'Allow')))
    [IO.File]::SetAccessControl($pf, $fs)
    $e = ErrorOf { Install-IemTuning -Root $er -SourceDir $src3 @h3 -KeepProfile }
    Assert ($e -like '*installed profile*' -and (Get-Installed $er) -ceq $installed) "tuning-install-keep-profile-refuses-a-user-writable-profile ($e)"

    # ---- the elevated root and junctions ----
    $plain = Join-Path $base 'plain-root'
    New-Item -ItemType Directory -Force -Path $plain | Out-Null
    $e = ErrorOf { Install-IemTuning -Root $plain -SourceDir $src @h }
    Assert ($e -like '*elevated root*' -and -not (Test-Path -LiteralPath (Join-Path $plain 'tuning'))) "tuning-install-refuses-a-root-that-is-not-admin-only ($e)"
    $outside = Join-Path $base 'outside'
    New-Item -ItemType Directory -Force -Path $outside | Out-Null
    $jr = New-ElevatedRoot 'er-junction'
    New-Item -ItemType Junction -Path (Join-Path $jr 'tuning') -Value $outside | Out-Null
    $e = ErrorOf { Install-IemTuning -Root $jr -SourceDir $src @h }
    Assert ($e -like '*junction or a link*' -and @(Get-ChildItem -LiteralPath $outside -Force).Count -eq 0) "tuning-install-refuses-a-junctioned-tuning-folder ($e)"
    $fr = New-ElevatedRoot 'er-file-junction'
    [void](Install-IemTuning -Root $fr -SourceDir $src @h)
    $before = Get-Installed $fr
    $jp = Join-Path (Join-Path $fr 'tuning') 'profile.json'
    Remove-Item -LiteralPath $jp -Force
    New-Item -ItemType Junction -Path $jp -Value $outside | Out-Null
    $h2own = Get-SourceHashes $src2
    $e = ErrorOf { Install-IemTuning -Root $fr -SourceDir $src2 @h2own }
    $mods = (FileSha (Join-Path (Join-Path $fr 'tuning') 'IemTuning.psm1')) + (FileSha (Join-Path (Join-Path $fr 'tuning') 'IemMeasure.psm1'))
    Assert ($e -like '*junction or a link*' -and $mods -ceq ($h.TuningSha256 + $h.MeasureSha256) -and
            @(Get-ChildItem -LiteralPath $outside -Force).Count -eq 0) "tuning-install-refuses-a-junction-in-place-of-a-file-and-writes-nothing ($e; before $before)"
} finally {
    Remove-Item -LiteralPath $base -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host 'Test-IemTuningInstall: all passed'
