#Requires -Version 5.1
# S6 PC module (design note section 5.1, section 6, section 7), shipped in every bundle:
# - bootstrap, run elevated over ssh (`iempc bootstrap <function>`); every
#   function is idempotent and reads back what it wrote;
# - the body of the elevated tasks (Invoke-IemTaskRequest);
# - the helpers of hil-v1.ps1.
# Split by responsibility (#36): this module keeps the small helpers, the
# elevated tasks' folder and body, S1c's tuning install, the firewall rule,
# the service right, the bundle sums and Defender, and the HIL runner; its
# siblings ($script:SiblingNames) hold the rest. It is still the one module
# every caller imports: it imports each sibling from its own folder, -Global
# (as IemTuning imports IemTuningStore, #34), so every caller reaches every
# function as before and the modules reach each other's through the global
# session state. Every admin-only copy of this module carries its siblings
# (Get-IemPcModuleFiles): the bootstrap's stage (iempc stages each one first,
# checked by the zip's sha256), <elevated root>\tasks (Install-IemElevatedDir),
# and the cutover's and the ssh shell undo's folders (their -ModuleSha256).
# Nothing is ended by force (I8). No site value lives here (P6): paths, the
# driver module, the preference key, the app and the service come as
# parameters from the private env or the site.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:ModuleFile = $PSCommandPath
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding $false
# Functions that `return ,$array` hand back exactly one array: assign their
# result (`$x = Test-IemBundleSums ...`); `@(Test-IemBundleSums ...)` would nest it.

# The siblings, in the order they are imported and staged (#36): the folders'
# DACLs, the scheduled tasks, the boot task's command line, Fast Startup, the
# preference and its holders, the bootstrap's read-only facts, HIL v1's
# helpers. iempc_bundle.PC_SIBLINGS lists the same names (a test compares them).
$script:SiblingNames = @('IemPcAcl.psm1', 'IemPcTasks.psm1', 'IemPcBootPref.psm1', 'IemPcFastStartup.psm1', 'IemPcPref.psm1', 'IemPcState.psm1', 'IemPcHilV1.psm1')
foreach ($iemSibling in $script:SiblingNames) { Import-Module (Join-Path $PSScriptRoot $iemSibling) -Force -Global }

# Service rights the user needs on the tunnel's service: RP start, WP stop, LO query.
$script:ServiceRights = 176
# The pinned runner (actions/runner release, win-x64).
$script:RunnerVersion = '2.337.0'
$script:RunnerSha256 = '1150692afa94e71f872017e254ea55b6eece1eece3fe7e3a6d4c93d0a1b85cfc'
$script:OpsUrl = 'https://github.com/zbynekdrlik/iemmixer-ops'
# The four verbs the tuning task accepts (design section 5.1).
$script:TuningVerbs = @('enter', 'exit', 'state', 'apply-tier2')
# The files Install-IemTuning puts into the elevated tuning folder (#15), in the
# order they go in: IemTuning imports IemTuningStore from its own folder (#34), so
# the store goes in first: a new IemTuning never meets an older store, or none on
# the first install after #34.
$script:TuningFiles = @('IemTuningStore.psm1', 'IemTuning.psm1', 'IemMeasure.psm1', 'profile.json')

# The elevated tasks' entry script, written by Register-IemTasks next to its
# copies of this module and its siblings (#36: the entry imports IemPc.psm1,
# which imports them from that folder). Its text is unchanged by the split.
$script:TaskEntry = @'
# iemmixer S6: the elevated tasks' entry (tuning, exclude, logon), written by
# Register-IemTasks next to its copy of IemPc.psm1 in <elevated root>\tasks
# (owner Administrators; only Administrators and SYSTEM may change it). It
# reads <Root>\guard\tasks\<kind>.request.json from the user's root and writes
# <elevated root>\tasks\out\<kind>.result.json, which the user may only read.
param([Parameter(Mandatory)][string]$Kind, [Parameter(Mandatory)][string]$Root,
      [Parameter(Mandatory)][string]$TuningDir,
      [string]$PrefKey = '', [string]$PrefName = '', [string]$PrefOriginal = '', [string]$Module = '')
# This process runs elevated: modules load only from Windows PowerShell's own
# folders, never from the user's Documents or an HKCU environment's path.
$pinned = [IO.Path]::Combine($PSHOME, 'Modules') + ';' + [IO.Path]::Combine([Environment]::GetFolderPath('ProgramFiles'), 'WindowsPowerShell\Modules')
$env:PSModulePath = $pinned
$ErrorActionPreference = 'Stop'
try {
    Import-Module ([IO.Path]::Combine($PSScriptRoot, 'IemPc.psm1')) -Force
    $out = [IO.Path]::Combine($PSScriptRoot, 'out')
    $r = Invoke-IemTaskRequest -Kind $Kind -Root $Root -OutDir $out -TuningDir $TuningDir -PrefKey $PrefKey -PrefName $PrefName -PrefOriginal $PrefOriginal -Module $Module
    if ($r.ok) { exit 0 }
    exit 1
} catch {
    [Console]::Error.WriteLine('iem-task: ' + $_.Exception.Message)
    exit 2
}
'@

# ---- small helpers ----

function Get-IemProp {
    # A property of a parsed JSON object, or $null (Set-StrictMode refuses a missing one).
    param($Object, [Parameter(Mandatory)][string]$Name)
    if ($null -eq $Object) { return $null }
    $p = $Object.PSObject.Properties[$Name]
    if ($null -eq $p) { return $null }
    return $p.Value
}

function Write-IemJsonFile {
    # JSON, UTF-8 without a BOM, written to a temp file and moved over the target.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)]$Value)
    $tmp = $Path + '.tmp'
    [IO.File]::WriteAllText($tmp, (ConvertTo-Json -InputObject $Value -Depth 8), $script:Utf8NoBom)
    Move-Item -LiteralPath $tmp -Destination $Path -Force
}

function Format-IemArg {
    # One command-line argument in double quotes. A value never holds a quote; a
    # trailing backslash would escape the closing one, so it is dropped.
    param([Parameter(Mandatory)][string]$Value)
    if ($Value.Contains('"')) { throw "argument refused (it holds a double quote): $Value" }
    return '"' + $Value.TrimEnd('\') + '"'
}

function Resolve-IemUser {
    # The account our tasks run as: the given name, or this session's token (over
    # ssh USERDOMAIN is the workgroup, so the token's own name is used).
    param([string]$User = '')
    if (-not $User) { $User = [Security.Principal.WindowsIdentity]::GetCurrent().Name }
    $sid = (New-Object Security.Principal.NTAccount $User).Translate([Security.Principal.SecurityIdentifier]).Value
    [pscustomobject]@{ name = $User; sid = $sid }
}

function Get-IemPcModuleFiles {
    # This module's files (#36): each sibling, in the order it is imported, then
    # IemPc.psm1 itself, all in the folder this module was imported from (an
    # elevated session's: the admin-only stage, the tasks folder, the cutover's
    # or the undo's folder). Every admin-only copy of IemPc.psm1 carries them
    # all, the siblings first: a copy cut off part-way never leaves a new
    # IemPc.psm1 without its siblings.
    $dir = [IO.Path]::GetDirectoryName($script:ModuleFile)
    $files = @()
    foreach ($name in $script:SiblingNames) { $files += [IO.Path]::Combine($dir, $name) }
    return @($files + @($script:ModuleFile))
}

function Install-IemElevatedDir {
    # The elevated root (design section 5.1; %ProgramData%\iemmixer on the PC):
    # tasks\ (this module, its siblings and the entry script), tasks\out\ (the
    # tasks' results) and tuning\ (S1c's module). Every folder is owned by
    # Administrators with a protected DACL (Administrators and SYSTEM change
    # it, the user only reads), our files are written fresh, and all of it
    # reads back. Each module file (Get-IemPcModuleFiles: the siblings first,
    # #36) is copied from the folder this module was imported from (in the
    # bootstrap, the admin-only stage, where iempc checked each by the zip's
    # sha256) and must hash as its source; a sibling missing there refuses
    # before the module and the entry are written.
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$UserSid)
    $tasks = Join-Path $Root 'tasks'
    foreach ($d in @($Root, $tasks, (Join-Path $tasks 'out'), (Join-Path $Root 'tuning'))) {
        Install-IemElevatedFolder -Path $d -UserSid $UserSid
    }
    $copies = [ordered]@{}
    foreach ($m in @(Get-IemPcModuleFiles)) { $copies[$m] = Join-Path $tasks (Split-Path -Leaf $m) }
    $entry = Join-Path $tasks 'iem-task.ps1'
    foreach ($m in @($copies.Keys)) {
        if ($copies[$m] -ne $m) { Write-IemElevatedFile -Path $copies[$m] -Bytes ([IO.File]::ReadAllBytes($m)) }
    }
    Write-IemElevatedFile -Path $entry -Bytes ($script:Utf8NoBom.GetBytes($script:TaskEntry))
    foreach ($f in @(@($copies.Values) + @($entry))) {
        $bad = Test-IemElevatedItem -Path $f -UserSid $UserSid
        if ($bad.Count -gt 0) { throw ('elevated file read-back: ' + ($bad -join '; ')) }
    }
    foreach ($m in @($copies.Keys)) {
        $h1 = (Get-FileHash -LiteralPath $m -Algorithm SHA256).Hash
        $h2 = (Get-FileHash -LiteralPath $copies[$m] -Algorithm SHA256).Hash
        if ($h1 -cne $h2) { throw "the copy of $(Split-Path -Leaf $m) in $tasks does not match the module" }
    }
    if ([IO.File]::ReadAllText($entry) -cne $script:TaskEntry) { throw "the entry script in $tasks does not read back" }
}

# ---- S1c's tuning module in the elevated root (#15) ----

function Get-IemBytesSha256 {
    # The SHA-256 of bytes in memory, as lowercase hex.
    param([Parameter(Mandatory)][AllowEmptyCollection()][byte[]]$Bytes)
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $h = $sha.ComputeHash($Bytes) } finally { $sha.Dispose() }
    return (-join @($h | ForEach-Object { $_.ToString('x2') }))
}

function Set-IemElevatedTemp {
    # TEMP and TMP of this elevated process at <Root>\temp, an admin-only folder
    # of the elevated root (Install-IemElevatedFolder, read back), before it
    # imports S1c's IemTuning.psm1: its Add-Type has csc write and then load a
    # DLL in TEMP, and the session user's TEMP is open to every process of the
    # user (#15). For the rest of the process. Returns the folder.
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$UserSid)
    $t = Join-Path $Root 'temp'
    Install-IemElevatedFolder -Path $t -UserSid $UserSid
    $env:TEMP = $t
    $env:TMP = $t
    return $t
}

function Remove-IemTuningStage {
    # Install-IemTuning's staging folder: the files it may hold (never through a
    # junction or a link), then the folder itself, which must then be empty.
    param([Parameter(Mandatory)][string]$Stage)
    if (-not (Test-Path -LiteralPath $Stage)) { return }
    if (Test-IemReparsePoint -Path $Stage) { throw "$Stage is a junction or a link: refused" }
    foreach ($name in $script:TuningFiles) {
        $p = Join-Path $Stage $name
        if (Test-IemReparsePoint -Path $p) { throw "$p is a junction or a link: refused" }
        if (Test-Path -LiteralPath $p) { [IO.File]::Delete($p) }
    }
    [IO.Directory]::Delete($Stage, $false)
}

function Install-IemTuning {
    # S1c's tuning modules and the private profile into <Root>\tuning, where the
    # tuning task loads them (Invoke-IemTuningVerb). Run elevated over ssh from
    # the dev box (`iempc tuning-install`, and `iempc activate`'s refresh): the
    # only path that installs them (#15, the decision of 2026-10-07: never from
    # the bundle in the user's root, which any process of the user may change).
    # -SourceDir holds what the dev box uploaded: IemTuning.psm1, its
    # IemTuningStore.psm1 (#34: IemTuning imports it from its own folder, so the
    # staged import below loads the staged store), IemMeasure.psm1 and, unless
    # -KeepProfile, profile.json. Each is read once from there and
    # its SHA-256 must be the one the dev box computed (the modules from the
    # attested bundle, the profile from the private file). Those checked bytes
    # go into an admin-only staging folder (<Root>\tuning-stage), and nothing is
    # read from the user's root again: IemTuning's own loader (Read-IemProfile,
    # Assert-IemLayout) checks the profile from there, the staged module
    # imported. -KeepProfile: the installed profile stays as it is, admin-only,
    # checked by the new module the same way. The root must be the admin-only
    # folder Register-IemTasks made; nothing is written through a junction or a
    # link. Any refusal leaves the tuning folder as it was and the staging
    # folder gone. Then each file goes in by a rename on the same volume, so the
    # tuning task (the guard's enter, exit, state) finds the old file or the new
    # one, never a part of one (between the delete and the rename, an instant,
    # none); a failure part-way leaves each file whole and is thrown, and a file
    # whose rename failed after its delete keeps its checked copy in the stage,
    # named in the error. Everything is read back (Test-IemElevatedItem, the
    # hashes). Returns the four hashes, never the profile's content (site
    # values, P6).
    param(
        [Parameter(Mandatory)][string]$SourceDir,
        [Parameter(Mandatory)][string]$TuningSha256,
        [Parameter(Mandatory)][string]$MeasureSha256,
        [Parameter(Mandatory)][string]$StoreSha256,
        [string]$ProfileSha256 = '',
        [switch]$KeepProfile,
        [string]$Root = '',
        [string]$User = ''
    )
    if ($KeepProfile.IsPresent -eq [bool]$ProfileSha256) {
        throw 'Install-IemTuning takes exactly one of -ProfileSha256 (a new profile) and -KeepProfile (the installed one)'
    }
    $Root = Resolve-IemElevatedRoot -ElevatedRoot $Root
    $u = Resolve-IemUser -User $User
    $tuning = Join-Path $Root 'tuning'
    $want = [ordered]@{ 'IemTuningStore.psm1' = $StoreSha256; 'IemTuning.psm1' = $TuningSha256; 'IemMeasure.psm1' = $MeasureSha256 }
    if (-not $KeepProfile) { $want['profile.json'] = $ProfileSha256 }
    $bytes = @{}
    foreach ($name in @($want.Keys)) {
        if ($want[$name] -cnotmatch '^[0-9a-f]{64}$') { throw "${name}: the expected SHA-256 is not 64 lowercase hex digits" }
        $b = [IO.File]::ReadAllBytes((Join-Path $SourceDir $name))
        $got = Get-IemBytesSha256 -Bytes $b
        if ($got -cne $want[$name]) { throw "${name}: sha256 $got, expected $($want[$name]): refused, nothing written" }
        $bytes[$name] = $b
    }
    $rootBad = Test-IemElevatedItem -Path $Root -UserSid $u.sid
    if ($rootBad.Count -gt 0) { throw ('the elevated root is refused (Register-IemTasks makes it): ' + ($rootBad -join '; ')) }
    $stage = Join-Path $Root 'tuning-stage'
    $targets = @{}
    $staged = @{}
    foreach ($name in $script:TuningFiles) { $targets[$name] = Join-Path $tuning $name; $staged[$name] = Join-Path $stage $name }
    foreach ($p in @($tuning, $stage) + @($targets.Values) + @($staged.Values)) {
        if (Test-IemReparsePoint -Path $p) { throw "$p is a junction or a link: refused, nothing written" }
    }
    $profilePath = $staged['profile.json']
    if ($KeepProfile) {
        $profilePath = $targets['profile.json']
        $bad = @()
        foreach ($p in @($tuning, $profilePath)) { $bad += Test-IemElevatedItem -Path $p -UserSid $u.sid }
        if ($bad.Count -gt 0) { throw ('the installed profile is refused (-KeepProfile needs an admin-only one): ' + ($bad -join '; ')) }
    }
    Install-IemElevatedFolder -Path $stage -UserSid $u.sid
    $swapping = $false
    try {
        foreach ($name in @($want.Keys)) { Write-IemElevatedFile -Path $staged[$name] -Bytes $bytes[$name] }
        [void](Set-IemElevatedTemp -Root $Root -UserSid $u.sid)
        Import-Module $staged['IemTuning.psm1'] -Force
        Assert-IemLayout -Profile (Read-IemProfile -Path $profilePath)
        Install-IemElevatedFolder -Path $tuning -UserSid $u.sid
        $swapping = $true
        foreach ($name in @($want.Keys)) {
            if (Test-IemReparsePoint -Path $targets[$name]) { throw "$($targets[$name]) is a junction or a link: refused" }
            if (Test-Path -LiteralPath $targets[$name]) { [IO.File]::Delete($targets[$name]) }
            [IO.File]::Move($staged[$name], $targets[$name])
        }
        $read = @{}
        foreach ($name in $script:TuningFiles) {
            $bad = Test-IemElevatedItem -Path $targets[$name] -UserSid $u.sid
            if ($bad.Count -gt 0) { throw ('tuning file read-back: ' + ($bad -join '; ')) }
            $read[$name] = (Get-FileHash -LiteralPath $targets[$name] -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($want.Contains($name) -and $read[$name] -cne $want[$name]) { throw "$name does not read back: sha256 $($read[$name])" }
        }
    } catch {
        $err = "$_"
        # A rename that failed after its target's delete: the checked copy stays
        # staged (named), never removed with the stage.
        $lost = @()
        if ($swapping) {
            $lost = @(@($want.Keys) | Where-Object { -not (Test-Path -LiteralPath $targets[$_]) -and (Test-Path -LiteralPath $staged[$_]) })
        }
        if ($lost.Count -gt 0) {
            $err += "; the tuning folder lacks $($lost -join ', '): the checked copies stay in $stage; run the install again"
        } else {
            try { Remove-IemTuningStage -Stage $stage } catch { $err += "; the staging folder $stage was not removed: $($_.Exception.Message)" }
        }
        throw $err
    }
    Remove-IemTuningStage -Stage $stage
    [pscustomobject]@{ store = $read['IemTuningStore.psm1']; tuning = $read['IemTuning.psm1']; measure = $read['IemMeasure.psm1']
                       profile = $read['profile.json'] }
}

# ---- firewall (P9) ----

function Get-IemFirewallRule {
    # A rule as plain values, or $null.
    param([Parameter(Mandatory)][string]$Name)
    $r = Get-NetFirewallRule -Name $Name -ErrorAction SilentlyContinue
    if ($null -eq $r) { return $null }
    $pf = $r | Get-NetFirewallPortFilter
    [pscustomobject]@{
        direction = "$($r.Direction)"
        action = "$($r.Action)"
        profile = ((@("$($r.Profile)" -split ',\s*') | Sort-Object) -join ',')
        enabled = "$($r.Enabled)"
        protocol = "$($pf.Protocol)"
        ports = ((@($pf.LocalPort) | ForEach-Object { "$_" } | Sort-Object) -join ',')
    }
}

function Test-IemFirewallRule {
    param($Rule, [Parameter(Mandatory)][string]$Enabled)
    if ($null -eq $Rule) { return $false }
    return ($Rule.direction -eq 'Inbound' -and $Rule.action -eq 'Allow' -and $Rule.profile -eq 'Domain,Private' -and
            $Rule.enabled -eq $Enabled -and $Rule.protocol -eq 'TCP' -and $Rule.ports -eq '443,80')
}

function Add-IemFirewallRule {
    # The server's one port rule (P9): TCP 80 and 443 inbound on the private and
    # domain profiles. -Disabled only for the self-test.
    param([string]$Name = 'iemmixer-http', [switch]$Disabled)
    $enabled = 'True'
    if ($Disabled) { $enabled = 'False' }
    $before = Get-IemFirewallRule -Name $Name
    $changed = $false
    if ($null -eq $before) {
        New-NetFirewallRule -Name $Name -DisplayName $Name -Direction Inbound -Action Allow -Protocol TCP -LocalPort 80, 443 `
            -Profile Domain, Private -Enabled $enabled | Out-Null
        $changed = $true
    } elseif (-not (Test-IemFirewallRule -Rule $before -Enabled $enabled)) {
        Set-NetFirewallRule -Name $Name -Direction Inbound -Action Allow -Protocol TCP -LocalPort 80, 443 -Profile Domain, Private -Enabled $enabled
        $changed = $true
    }
    $after = Get-IemFirewallRule -Name $Name
    if (-not (Test-IemFirewallRule -Rule $after -Enabled $enabled)) {
        throw ('firewall rule {0} reads back as {1}' -f $Name, (ConvertTo-Json -InputObject $after -Compress))
    }
    [pscustomobject]@{ name = $Name; changed = $changed; rule = $after }
}

# ---- service rights (S0 hand-off 7: the tunnel's service) ----

function Get-IemServiceSddl {
    param([Parameter(Mandatory)][string]$Service)
    $ErrorActionPreference = 'Continue'
    $out = @(& sc.exe sdshow $Service 2>&1)
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($code -ne 0) { throw ('sc.exe sdshow {0} exited {1}: {2}' -f $Service, $code, ($out -join ' ')) }
    $lines = @($out | ForEach-Object { "$_".Trim() } | Where-Object { $_ -cmatch '^(O:|G:|D:|S:)' })
    if ($lines.Count -ne 1) { throw ('sc.exe sdshow {0}: no security descriptor in "{1}"' -f $Service, ($out -join ' ')) }
    return $lines[0]
}

function Test-IemServiceGrant {
    # Whether a service DACL lets $Sid itself start, stop and query it (allowed,
    # none denied). A grant check, not an exact read-back: a service DACL has no
    # parent to inherit from; an inherit-only ACE (IO) does not apply to the
    # service and is skipped, every other ACE for $Sid counts.
    param([Parameter(Mandatory)][string]$Sddl, [Parameter(Mandatory)][string]$Sid)
    $sd = New-Object System.Security.AccessControl.RawSecurityDescriptor $Sddl
    $allow = 0
    $deny = 0
    if ($null -ne $sd.DiscretionaryAcl) {
        foreach ($ace in $sd.DiscretionaryAcl) {
            if ($ace -isnot [System.Security.AccessControl.CommonAce]) { continue }
            if ($ace.SecurityIdentifier.Value -ne $Sid) { continue }
            if (([int]$ace.AceFlags -band 8) -ne 0) { continue }
            if ($ace.AceQualifier -eq [System.Security.AccessControl.AceQualifier]::AccessAllowed) { $allow = $allow -bor $ace.AccessMask }
            elseif ($ace.AceQualifier -eq [System.Security.AccessControl.AceQualifier]::AccessDenied) { $deny = $deny -bor $ace.AccessMask }
        }
    }
    return ((($allow -band $script:ServiceRights) -eq $script:ServiceRights) -and (($deny -band $script:ServiceRights) -eq 0))
}

function Add-IemServiceAce {
    # The descriptor with one more ACE: (A;;RPWPLO;;;<sid>).
    param([Parameter(Mandatory)][string]$Sddl, [Parameter(Mandatory)][string]$Sid)
    $sd = New-Object System.Security.AccessControl.RawSecurityDescriptor $Sddl
    if ($null -eq $sd.DiscretionaryAcl) { throw 'the service has no DACL' }
    $id = New-Object System.Security.Principal.SecurityIdentifier $Sid
    $ace = New-Object System.Security.AccessControl.CommonAce([System.Security.AccessControl.AceFlags]::None,
        [System.Security.AccessControl.AceQualifier]::AccessAllowed, $script:ServiceRights, $id, $false, $null)
    $sd.DiscretionaryAcl.InsertAce($sd.DiscretionaryAcl.Count, $ace)
    return $sd.GetSddlForm([System.Security.AccessControl.AccessControlSections]::All)
}

function Test-IemServiceRight {
    # Read-only: may the user start, stop and query the service?
    param([Parameter(Mandatory)][string]$Service, [string]$User = '')
    $u = Resolve-IemUser -User $User
    $sddl = Get-IemServiceSddl -Service $Service
    [pscustomobject]@{ service = $Service; user = $u.name; granted = (Test-IemServiceGrant -Sddl $sddl -Sid $u.sid); sddl = $sddl }
}

function Grant-IemServiceRight {
    # Lets the user start, stop and query the service; prints the descriptor
    # before and after.
    param([Parameter(Mandatory)][string]$Service, [string]$User = '')
    $u = Resolve-IemUser -User $User
    $before = Get-IemServiceSddl -Service $Service
    $changed = $false
    if (-not (Test-IemServiceGrant -Sddl $before -Sid $u.sid)) {
        $new = Add-IemServiceAce -Sddl $before -Sid $u.sid
        $ErrorActionPreference = 'Continue'
        $out = @(& sc.exe sdset $Service $new 2>&1)
        $code = $LASTEXITCODE
        $ErrorActionPreference = 'Stop'
        if ($code -ne 0) { throw ('sc.exe sdset {0} exited {1}: {2}' -f $Service, $code, ($out -join ' ')) }
        $changed = $true
    }
    $after = Get-IemServiceSddl -Service $Service
    $granted = Test-IemServiceGrant -Sddl $after -Sid $u.sid
    if (-not $granted) { throw ('service {0}: the grant does not read back ({1})' -f $Service, $after) }
    [pscustomobject]@{ service = $Service; user = $u.name; changed = $changed; granted = $granted; before = $before; after = $after }
}

# ---- bundles and Defender (P5, S1c G4) ----

function Test-IemBundleSums {
    # A bundle directory against its SHA256SUMS, as the guard's bundle.rs: every
    # listed file present with its hash, nothing unlisted (the sums file itself
    # exempt), names a root file or one level under tuning/. Returns the names.
    param([Parameter(Mandatory)][string]$Dir)
    $base = (Resolve-Path -LiteralPath $Dir).ProviderPath.TrimEnd('\')
    $listed = @{}
    foreach ($line in ([IO.File]::ReadAllText((Join-Path $base 'SHA256SUMS')) -split "`n")) {
        $l = $line.TrimEnd("`r")
        if (-not $l) { continue }
        if ($l -cnotmatch '^([0-9a-f]{64})  ([A-Za-z0-9_.-]+(/[A-Za-z0-9_.-]+)?)$') { throw "malformed SHA256SUMS line: $l" }
        $sum = $Matches[1]
        $name = $Matches[2]
        $parts = @($name -split '/')
        if ($name.Contains('..') -or $parts -contains '.' -or $name -eq 'SHA256SUMS' -or ($parts.Count -eq 2 -and $parts[0] -cne 'tuning')) {
            throw "SHA256SUMS name refused: $name"
        }
        if ($listed.ContainsKey($name)) { throw "SHA256SUMS lists $name twice" }
        $listed[$name] = $sum
    }
    $present = @{}
    foreach ($f in (Get-ChildItem -LiteralPath $base -Recurse -File -Force)) {
        $rel = $f.FullName.Substring($base.Length + 1).Replace('\', '/')
        if ($rel -eq 'SHA256SUMS') { continue }
        if (-not $listed.ContainsKey($rel)) { throw "not in SHA256SUMS: $rel" }
        $h = (Get-FileHash -LiteralPath $f.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($h -cne $listed[$rel]) { throw "does not match SHA256SUMS: $rel" }
        $present[$rel] = $true
    }
    foreach ($name in $listed.Keys) { if (-not $present.ContainsKey($name)) { throw "listed in SHA256SUMS but missing: $name" } }
    $names = @($listed.Keys | Sort-Object)
    return ,$names
}

function Get-IemExclusionPlan {
    # Defender process exclusions under <Root>\bundles\: exactly the verified
    # bundle's executables, the kept bundles' stay, every other one goes. An
    # exclusion outside <Root>\bundles\ is never touched.
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$Sha, [string[]]$Want = @(),
          [string[]]$Keep = @(), [string[]]$Current = @())
    $prefix = (Join-Path $Root 'bundles').TrimEnd('\') + '\'
    $add = @()
    $remove = @()
    foreach ($w in $Want) { if (-not ($Current -contains $w)) { $add += $w } }
    foreach ($c in $Current) {
        if (-not $c -or -not $c.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) { continue }
        $dir = @($c.Substring($prefix.Length) -split '\\')[0].ToLowerInvariant()
        if ($dir -ceq $Sha) {
            if (-not ($Want -contains $c)) { $remove += $c }
        } elseif (-not ($Keep -contains $dir)) {
            $remove += $c
        }
    }
    [pscustomobject]@{ add = $add; remove = $remove }
}

function Set-IemDefenderExclusion {
    # S1c G4, run by the exclude task for an activated bundle: re-verifies
    # bundles\<sha>\SHA256SUMS itself, then Defender process exclusions for
    # exactly that directory's executables (full paths); those of bundles that
    # are neither this one nor -Keep (current, previous) go. Never a folder
    # exclusion on the user-writable root (any found is reported).
    [CmdletBinding(SupportsShouldProcess)]
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$Sha, [string[]]$Keep = @())
    foreach ($s in (@($Sha) + @($Keep))) { if ($s -cnotmatch '^[0-9a-f]{40}$') { throw "not a bundle SHA: '$s'" } }
    $bundles = Join-Path $Root 'bundles'
    $dir = Join-Path $bundles $Sha
    # The exclude task runs elevated in the user's root: never through a junction or a link.
    foreach ($p in @($Root, $bundles, $dir)) { if (Test-IemReparsePoint -Path $p) { throw "$p is a junction or a link: refused" } }
    $names = Test-IemBundleSums -Dir $dir
    $want = @($names | Where-Object { $_ -notlike '*/*' -and $_ -like '*.exe' } | ForEach-Object { Join-Path $dir $_ })
    if ($want.Count -eq 0) { throw "bundle $Sha has no executable" }
    $defender = 'available'
    $current = @()
    $folders = @()
    try {
        $pref = Get-MpPreference
        $current = @($pref.ExclusionProcess | Where-Object { $_ })
        $rootPath = $Root.TrimEnd('\')
        $folders = @($pref.ExclusionPath | Where-Object { $_ -and $_.StartsWith($rootPath, [StringComparison]::OrdinalIgnoreCase) })
    } catch {
        if (-not $WhatIfPreference) { throw ('Defender preferences unreadable: ' + $_.Exception.Message) }
        $defender = 'unavailable: ' + $_.Exception.Message
    }
    $plan = Get-IemExclusionPlan -Root $Root -Sha $Sha -Want $want -Keep $Keep -Current $current
    foreach ($p in $plan.remove) { if ($PSCmdlet.ShouldProcess($p, 'remove the Defender process exclusion')) { Remove-MpPreference -ExclusionProcess $p } }
    foreach ($p in $plan.add) { if ($PSCmdlet.ShouldProcess($p, 'add a Defender process exclusion')) { Add-MpPreference -ExclusionProcess $p } }
    if (-not $WhatIfPreference) {
        $now = @((Get-MpPreference).ExclusionProcess | Where-Object { $_ })
        $left = Get-IemExclusionPlan -Root $Root -Sha $Sha -Want $want -Keep $Keep -Current $now
        if ($left.add.Count -gt 0 -or $left.remove.Count -gt 0) {
            throw ('Defender read-back: missing {0}; still excluded {1}' -f ($left.add -join ', '), ($left.remove -join ', '))
        }
    }
    [pscustomobject]@{ sha = $Sha; add = $plan.add; remove = $plan.remove; keep = $Keep; whatif = [bool]$WhatIfPreference
                       defender = $defender; folder_exclusions = $folders }
}

# ---- the HIL runner (design section 7) ----

function Register-IemRunner {
    # The pinned runner, registered on the ops repo with the label iem-pc. The
    # one-time registration token comes only in the process environment variable
    # ACTIONS_RUNNER_INPUT_TOKEN (config.cmd reads it), never on a command line,
    # and is removed right after. No service: the guard starts the runner in dev
    # only. -WhatIf plans without touching anything.
    [CmdletBinding(SupportsShouldProcess)]
    param([Parameter(Mandatory)][string]$Dir, [string]$Zip = '', [string]$Version = '', [string]$Sha256 = '',
          [string]$Url = '', [string]$Label = 'iem-pc')
    if (-not $Version) { $Version = $script:RunnerVersion }
    if (-not $Sha256) { $Sha256 = $script:RunnerSha256 }
    if (-not $Url) { $Url = $script:OpsUrl }
    if ($Version -cnotmatch '^[0-9]+\.[0-9]+\.[0-9]+$') { throw "runner version '$Version' refused" }
    if ($Sha256 -cnotmatch '^[0-9a-f]{64}$') { throw 'the runner zip needs its pinned sha256 (64 lowercase hex)' }
    if ($Url -cnotmatch '^https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') { throw "runner URL '$Url' refused" }
    if ($Label -cnotmatch '^[A-Za-z0-9_-]+$') { throw "runner label '$Label' refused" }
    $Dir = $Dir.TrimEnd('\')
    if (-not $Zip) { $Zip = Join-Path (Split-Path -Parent $Dir) ('actions-runner-win-x64-{0}.zip' -f $Version) }
    $configArgs = @('--unattended', '--url', $Url, '--labels', $Label, '--work', '_work', '--replace')
    $tokenSet = -not [string]::IsNullOrEmpty($env:ACTIONS_RUNNER_INPUT_TOKEN)
    $marker = Join-Path $Dir '.runner'
    if (Test-Path -LiteralPath $marker -PathType Leaf) {
        $cfg = [IO.File]::ReadAllText($marker) | ConvertFrom-Json
        $registered = [string](Get-IemProp $cfg 'gitHubUrl')
        Remove-Item -Path 'Env:\ACTIONS_RUNNER_INPUT_TOKEN' -ErrorAction SilentlyContinue
        if ($registered.TrimEnd('/') -cne $Url) { throw "the runner in $Dir is registered for '$registered', not $Url" }
        return [pscustomobject]@{ dir = $Dir; configured = $true; changed = $false; url = $registered; version = $Version
                                  args = $configArgs; token_present = $tokenSet; whatif = [bool]$WhatIfPreference }
    }
    if (-not $PSCmdlet.ShouldProcess($Dir, "download runner $Version, check its sha256, unpack and register it for $Url")) {
        return [pscustomobject]@{ dir = $Dir; configured = $false; changed = $false; url = $Url; version = $Version; zip = $Zip
                                  args = $configArgs; token_present = $tokenSet; whatif = $true }
    }
    if (-not $tokenSet) { throw 'the one-time registration token is missing from ACTIONS_RUNNER_INPUT_TOKEN' }
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        if (-not (Test-Path -LiteralPath $Zip -PathType Leaf)) {
            $part = $Zip + '.partial'
            $src = 'https://github.com/actions/runner/releases/download/v{0}/actions-runner-win-x64-{0}.zip' -f $Version
            Invoke-WebRequest -UseBasicParsing -Uri $src -OutFile $part
            Move-Item -LiteralPath $part -Destination $Zip -Force
        }
        $h = (Get-FileHash -LiteralPath $Zip -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($h -cne $Sha256) { throw ('runner zip {0} has sha256 {1}, the pin is {2}' -f $Zip, $h, $Sha256) }
        if (-not (Test-Path -LiteralPath $Dir -PathType Container)) { New-Item -ItemType Directory -Force -Path $Dir | Out-Null }
        Expand-Archive -LiteralPath $Zip -DestinationPath $Dir -Force
        $ErrorActionPreference = 'Continue'
        $out = @(& (Join-Path $Dir 'config.cmd') @configArgs 2>&1)
        $code = $LASTEXITCODE
        $ErrorActionPreference = 'Stop'
        if ($code -ne 0) { throw ('config.cmd exited {0}: {1}' -f $code, (@($out | Select-Object -Last 20) -join ' | ')) }
    } finally {
        Remove-Item -Path 'Env:\ACTIONS_RUNNER_INPUT_TOKEN' -ErrorAction SilentlyContinue
    }
    if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) { throw "config.cmd left no $marker" }
    $registered = [string](Get-IemProp ([IO.File]::ReadAllText($marker) | ConvertFrom-Json) 'gitHubUrl')
    if ($registered.TrimEnd('/') -cne $Url) { throw "the runner reads back as registered for '$registered'" }
    [pscustomobject]@{ dir = $Dir; configured = $true; changed = $true; url = $registered; version = $Version
                       args = $configArgs; token_present = $tokenSet; whatif = $false }
}

# ---- the elevated tasks' body (design section 5.1) ----

function Get-IemTaskUserSid {
    # The account this task runs as: the user whose rights the elevated root grants.
    return [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
}

function Read-IemTaskRequest {
    # <Root>\guard\tasks\<kind>.request.json, written by the Limited guard in the
    # user's root: never read through a junction or a link, at most 64 KiB, and
    # only its own fields are used.
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$Kind)
    $guard = Join-Path $Root 'guard'
    $dir = Join-Path $guard 'tasks'
    $path = Join-Path $dir ($Kind + '.request.json')
    foreach ($p in @($Root, $guard, $dir, $path)) { if (Test-IemReparsePoint -Path $p) { throw "$p is a junction or a link: refused" } }
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "no request at $path" }
    if ((Get-Item -LiteralPath $path -Force).Length -gt 65536) { throw 'the request is larger than 64 KiB' }
    $doc = $null
    try { $doc = [IO.File]::ReadAllText($path) | ConvertFrom-Json } catch { throw 'the request is not JSON' }
    $id = [string](Get-IemProp $doc 'id')
    if ($id -cnotmatch '^[A-Za-z0-9_.:-]{1,64}$') { throw 'request id refused (1 to 64 of A-Z a-z 0-9 _ . : -)' }
    [pscustomobject]@{ id = $id; doc = $doc }
}

function Invoke-IemTuningVerb {
    # S1c's tuning module: IemTuning.psm1 and profile.json in -TuningDir
    # (<elevated root>\tuning, from the task's command line); 'absent' until S1c
    # ships them. The module runs elevated, so it is imported only when its
    # folder, that folder's parent, the module and the profile are all owned
    # by Administrators or SYSTEM and only they may change them. IemTuning
    # imports IemTuningStore.psm1 from the same folder (#34): read back the same
    # way whenever anything is there by that name (a dangling link too). A
    # folder from before #34 holds none and its IemTuning needs none; a newer
    # IemTuning without it fails its own import.
    param([Parameter(Mandatory)][string]$Verb, [string]$TuningDir = '')
    if ($script:TuningVerbs -cnotcontains $Verb) { throw "tuning verb '$Verb' refused (enter, exit, state, apply-tier2)" }
    $TuningDir = $TuningDir.TrimEnd('\')
    if (-not $TuningDir -or -not [IO.Path]::IsPathRooted($TuningDir)) { throw 'the tuning folder (-TuningDir) is not an absolute path' }
    $module = Join-Path $TuningDir 'IemTuning.psm1'
    $store = Join-Path $TuningDir 'IemTuningStore.psm1'
    $profilePath = Join-Path $TuningDir 'profile.json'
    if (-not (Test-Path -LiteralPath $module -PathType Leaf) -or -not (Test-Path -LiteralPath $profilePath -PathType Leaf)) { return 'absent' }
    $sid = Get-IemTaskUserSid
    $check = @((Split-Path -Parent $TuningDir), $TuningDir, $module, $profilePath)
    if ((Test-Path -LiteralPath $store) -or (Test-IemReparsePoint -Path $store)) { $check += $store }
    $bad = @()
    foreach ($p in $check) {
        $b = Test-IemElevatedItem -Path $p -UserSid $sid
        $bad += $b
    }
    if ($bad.Count -gt 0) { throw ('tuning module refused (not admin-only): ' + ($bad -join '; ')) }
    [void](Set-IemElevatedTemp -Root (Split-Path -Parent $TuningDir) -UserSid $sid)
    Import-Module $module -Force
    switch ($Verb) {
        'enter' { return (Enter-IemTuningMode -ProfilePath $profilePath) }
        'exit' { return (Exit-IemTuningMode -ProfilePath $profilePath) }
        'state' { return (Get-IemTuningState -ProfilePath $profilePath) }
        'apply-tier2' { return (Invoke-IemTuningApply -ProfilePath $profilePath -Tier 2) }
    }
}

function Invoke-IemTaskRequest {
    # The body of the Highest tasks. The guard writes <Root>\guard\tasks\
    # <kind>.request.json ({"id", "verb"} for tuning; {"id", "sha", "keep"} for
    # exclude), runs the task and reads -OutDir\<kind>.result.json ({"kind",
    # "id", "ok", "at", "result", "error"}; -OutDir is <elevated root>\tasks\out,
    # which only Administrators and SYSTEM may change, so the elevated write
    # never lands in the user's root). logon (at the user's logon, G1): tuning
    # exit, then the preference back to its original, never while a process
    # holds the driver module -Module (Restore-IemPref; a value left under a
    # holder is named in the result, which the guard reads).
    param([Parameter(Mandatory)][ValidateSet('tuning', 'exclude', 'logon')][string]$Kind, [Parameter(Mandatory)][string]$Root,
          [Parameter(Mandatory)][string]$OutDir, [string]$TuningDir = '', [string]$PrefKey = '', [string]$PrefName = '',
          [string]$PrefOriginal = '', [string]$Module = '')
    $outBad = Test-IemElevatedItem -Path $OutDir -UserSid (Get-IemTaskUserSid)
    if ($outBad.Count -gt 0) { throw ('the result folder is refused (not admin-only): ' + ($outBad -join '; ')) }
    $outFile = Join-Path $OutDir ($Kind + '.result.json')
    foreach ($p in @($outFile, ($outFile + '.tmp'))) { if (Test-IemReparsePoint -Path $p) { throw "$p is a junction or a link: refused" } }
    $result = [ordered]@{ kind = $Kind; id = ''; ok = $false; at = (Get-Date).ToUniversalTime().ToString('o'); result = $null; error = '' }
    try {
        switch ($Kind) {
            'tuning' {
                $req = Read-IemTaskRequest -Root $Root -Kind 'tuning'
                $result.id = $req.id
                $result.result = Invoke-IemTuningVerb -Verb ([string](Get-IemProp $req.doc 'verb')) -TuningDir $TuningDir
                $result.ok = $true
            }
            'exclude' {
                $req = Read-IemTaskRequest -Root $Root -Kind 'exclude'
                $result.id = $req.id
                $keep = @(Get-IemProp $req.doc 'keep' | Where-Object { $null -ne $_ } | ForEach-Object { [string]$_ })
                $result.result = Set-IemDefenderExclusion -Root $Root -Sha ([string](Get-IemProp $req.doc 'sha')) -Keep $keep
                $result.ok = $true
            }
            'logon' {
                $result.id = 'logon'
                $tuning = ''
                try { $tuning = Invoke-IemTuningVerb -Verb 'exit' -TuningDir $TuningDir } catch { $tuning = 'failed: ' + $_.Exception.Message }
                if (-not $PrefKey -or -not $PrefName -or -not $PrefOriginal -or -not $Module) {
                    throw 'the logon task needs -PrefKey, -PrefName, -PrefOriginal and -Module'
                }
                $pref = Restore-IemPref -Key $PrefKey -Name $PrefName -Original $PrefOriginal -Module $Module
                $result.result = [pscustomobject]@{ tuning = $tuning; pref = $pref }
                # A value left under a holder is no failure of the task: the
                # result names it (action held), the guard reads it (#9 2026-09-28).
                $result.ok = [bool]($pref.ok -or $pref.action -eq 'held')
            }
        }
    } catch {
        $result.error = $_.Exception.Message
    }
    $out = [pscustomobject]$result
    Write-IemJsonFile -Path $outFile -Value $out
    return $out
}

Export-ModuleMember -Function *-Iem*
