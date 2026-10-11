#Requires -Version 5.1
# IemPc.psm1's folders and their DACLs (design note section 6; split out of
# IemPc.psm1 by responsibility, #36): the user's root and everything below it
# (Set-IemRootAcl, Test-IemRootTree, Reset-IemInheritedItem), and the
# admin-only folders and files of the elevated root (Install-IemElevatedFolder,
# Write-IemElevatedFile, Test-IemElevatedItem).
# IemPc.psm1 imports this module from its own folder (-Global, as IemTuning
# imports IemTuningStore, #34), so it goes wherever IemPc.psm1 goes: the
# bundle, the bootstrap's admin-only stage, <elevated root>\tasks and the
# cutover's and the ssh shell undo's folders. Its functions reach the other
# IemPc modules' through the global session state. Nothing is ended by force
# (I8). No site value lives here (P6).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:SidAdmins = 'S-1-5-32-544'
$script:SidSystem = 'S-1-5-18'

# ---- folders and their DACLs (design section 6) ----

function Get-IemRootRights {
    # The PC root: the user, SYSTEM and Administrators, full control.
    param([Parameter(Mandatory)][string]$UserSid)
    $full = [System.Security.AccessControl.FileSystemRights]::FullControl
    $r = @{}
    $r[$UserSid] = $full
    $r[$script:SidSystem] = $full
    $r[$script:SidAdmins] = $full
    return $r
}

function Get-IemElevatedRights {
    # The Highest tasks' folder: Administrators and SYSTEM change it, the user only reads.
    param([Parameter(Mandatory)][string]$UserSid)
    $r = @{}
    $r[$script:SidAdmins] = [System.Security.AccessControl.FileSystemRights]::FullControl
    $r[$script:SidSystem] = [System.Security.AccessControl.FileSystemRights]::FullControl
    $r[$UserSid] = [System.Security.AccessControl.FileSystemRights]::ReadAndExecute
    return $r
}

function Set-IemDirectoryAcl {
    # A protected DACL (nothing inherited from above) that everything below inherits.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][hashtable]$Rights)
    $acl = New-Object System.Security.AccessControl.DirectorySecurity
    $acl.SetAccessRuleProtection($true, $false)
    foreach ($sid in $Rights.Keys) {
        $id = New-Object System.Security.Principal.SecurityIdentifier $sid
        $rule = New-Object System.Security.AccessControl.FileSystemAccessRule($id, $Rights[$sid], 'ContainerInherit, ObjectInherit', 'None', 'Allow')
        $acl.AddAccessRule($rule)
    }
    [System.IO.Directory]::SetAccessControl($Path, $acl)
}

function Test-IemDirectoryAcl {
    # The DACL read back against Set-IemDirectoryAcl; returns the differences.
    # Exact: that DACL is protected, so it inherits nothing, and an inherited
    # rule (whoever it names) is a difference like any other.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][hashtable]$Rights)
    $acl = Get-Acl -LiteralPath $Path
    $bad = @()
    if (-not $acl.AreAccessRulesProtected) { $bad += 'inherits from its parent' }
    $seen = @{}
    foreach ($r in @($acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))) {
        $sid = $r.IdentityReference.Value
        if (-not $Rights.ContainsKey($sid)) { $bad += "a rule for $sid"; continue }
        if ("$($r.AccessControlType)" -ne 'Allow') { $bad += "a deny rule for $sid"; continue }
        if ($r.IsInherited) { $bad += "an inherited rule for $sid" }
        # An allow rule always carries Synchronize (1048576).
        $want = ([int]$Rights[$sid]) -bor 1048576
        if ([int]$r.FileSystemRights -ne $want -or [int]$r.InheritanceFlags -ne 3 -or [int]$r.PropagationFlags -ne 0) {
            $bad += ('{0}: {1} ({2}, {3})' -f $sid, $r.FileSystemRights, $r.InheritanceFlags, $r.PropagationFlags)
        }
        if ($seen.ContainsKey($sid)) { $bad += "two rules for $sid" }
        $seen[$sid] = $true
    }
    foreach ($sid in $Rights.Keys) { if (-not $seen.ContainsKey($sid)) { $bad += "no rule for $sid" } }
    return ,$bad
}

function Test-IemInheritedItem {
    # A file or folder below the root read back against the root's DACL (design
    # section 6): it holds nothing of its own (no explicit rule, not protected)
    # and inherits exactly $Rights, one allow rule per SID (a folder's rules
    # pass on to its own children). Returns the differences.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][hashtable]$Rights)
    $acl = Get-Acl -LiteralPath $Path
    $flags = 0
    if ($acl -is [System.Security.AccessControl.DirectorySecurity]) { $flags = 3 }
    $bad = @()
    if ($acl.AreAccessRulesProtected) { $bad += "${Path}: inherits nothing from its folder" }
    $seen = @{}
    foreach ($r in @($acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))) {
        $sid = $r.IdentityReference.Value
        if (-not $r.IsInherited) { $bad += "${Path}: a rule of its own for $sid"; continue }
        if (-not $Rights.ContainsKey($sid)) { $bad += "${Path}: an inherited rule for $sid"; continue }
        if ("$($r.AccessControlType)" -ne 'Allow') { $bad += "${Path}: a deny rule for $sid"; continue }
        # An allow rule always carries Synchronize (1048576).
        $want = ([int]$Rights[$sid]) -bor 1048576
        if ([int]$r.FileSystemRights -ne $want -or [int]$r.InheritanceFlags -ne $flags -or [int]$r.PropagationFlags -ne 0) {
            $bad += ('{0}: {1}: {2} ({3}, {4})' -f $Path, $sid, $r.FileSystemRights, $r.InheritanceFlags, $r.PropagationFlags)
        }
        if ($seen.ContainsKey($sid)) { $bad += "${Path}: two rules for $sid" }
        $seen[$sid] = $true
    }
    foreach ($sid in $Rights.Keys) { if (-not $seen.ContainsKey($sid)) { $bad += "${Path}: no rule for $sid" } }
    return ,$bad
}

function Test-IemRootTree {
    # Every file and folder below the root (design section 6), parents before
    # their children, read back with Test-IemInheritedItem. A junction or a
    # link is a difference and is never followed or read through (Set-IemRootAcl
    # refuses one before it writes anything). Returns one row per item that
    # differs: {path, rel (below the root), directory, link, problems}.
    # -LinksOnly reads no DACL and returns only the junctions and links.
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][hashtable]$Rights, [switch]$LinksOnly)
    $base = (Resolve-Path -LiteralPath $Root).ProviderPath.TrimEnd('\')
    $rows = @()
    $dirs = New-Object System.Collections.ArrayList
    [void]$dirs.Add($base)
    for ($i = 0; $i -lt $dirs.Count; $i++) {
        foreach ($item in @(Get-ChildItem -LiteralPath ([string]$dirs[$i]) -Force)) {
            # Built from the root's own spelling, so `rel` is exactly the part
            # below it (Reset-IemInheritedItem builds the path back from it).
            $path = ([string]$dirs[$i]) + '\' + $item.Name
            $isDir = [bool]$item.PSIsContainer
            $row = [pscustomobject]@{ path = $path; rel = $path.Substring($base.Length + 1); directory = $isDir; link = $false; problems = @() }
            if (Test-IemReparsePoint -Path $path) {
                $row.link = $true
                $row.problems = @("$path is a junction or a link")
                $rows += $row
                continue
            }
            # Gone since the listing (another process removed or renamed it).
            if (-not (Test-Path -LiteralPath $path)) { continue }
            if (-not $LinksOnly) {
                $bad = Test-IemInheritedItem -Path $path -Rights $Rights
                if ($bad.Count -gt 0) { $row.problems = $bad; $rows += $row }
            }
            if ($isDir) { [void]$dirs.Add($path) }
        }
    }
    return ,$rows
}

function Reset-IemInheritedItem {
    # A file or folder below the root keeps nothing of its own and inherits its
    # folder's rules again (icacls /reset for this one item): 'D:' is an empty
    # DACL, present and not protected, so what SetAccessControl leaves on the
    # item is exactly what it inherits. $Rel is the item below $Root, as
    # Test-IemRootTree gives it. SetAccessControl goes by path, so right before
    # it every part from the root down to the item is checked for a junction
    # or a link, as the elevated reads in the user's root do
    # (Read-IemTaskRequest): a folder swapped for a junction since the walk is
    # refused, never written through. That narrows the race to the moment of
    # use; a path-based write cannot close it.
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$Rel, [switch]$Directory)
    $names = $Rel.Split('\')
    if (@($names | Where-Object { $_ -eq '' -or $_ -eq '.' -or $_ -eq '..' -or $_.Contains(':') -or $_.Contains('/') }).Count -gt 0) {
        throw "not an item below the root: '$Rel'"
    }
    $path = (Resolve-Path -LiteralPath $Root).ProviderPath.TrimEnd('\')
    $parts = @($path)
    foreach ($n in $names) { $path = $path + '\' + $n; $parts += $path }
    foreach ($p in $parts) { if (Test-IemReparsePoint -Path $p) { throw "$p is a junction or a link: refused" } }
    if ($Directory) { $sec = New-Object System.Security.AccessControl.DirectorySecurity } else { $sec = New-Object System.Security.AccessControl.FileSecurity }
    $sec.SetSecurityDescriptorSddlForm('D:', [System.Security.AccessControl.AccessControlSections]::Access)
    if ($Directory) { [IO.Directory]::SetAccessControl($path, $sec) } else { [IO.File]::SetAccessControl($path, $sec) }
}

function Set-IemRootAcl {
    # The PC root (%LOCALAPPDATA%\iemmixer, design section 6): a protected DACL for the
    # user, SYSTEM and Administrators that everything below inherits, so the
    # staging copies of `iem-migrate band` need no ACL work (#20). Setting the
    # root's DACL is not enough for what already exists below it: on the CI
    # runner (run 36366619573) a file that existed before failed its read-back
    # afterwards (the log does not show its rules), and by design Windows'
    # propagation keeps the rules an item holds of its own and passes nothing
    # into a protected item. So every item below is read back
    # (Test-IemRootTree) and each one that differs is reset to inherit only
    # (Reset-IemInheritedItem), parents first, then all of it is read back.
    # Setting the root's DACL or resetting a folder makes Windows propagate to
    # what lies below, and whether that goes through a junction is not relied
    # on: a root that is a junction or a link, or any junction or link below
    # it, is refused before anything is written, and never followed. `reset`
    # lists the items reset, relative to the root; `differed` what they held,
    # read right before each reset (after the root's DACL was set), the record
    # of what the root's DACL alone did not fix.
    param([Parameter(Mandatory)][string]$Root, [string]$User = '')
    $u = Resolve-IemUser -User $User
    if (Test-IemReparsePoint -Path $Root) { throw "$Root is a junction or a link: refused" }
    if (-not (Test-Path -LiteralPath $Root -PathType Container)) { New-Item -ItemType Directory -Force -Path $Root | Out-Null }
    $rights = Get-IemRootRights -UserSid $u.sid
    $links = Test-IemRootTree -Root $Root -Rights $rights -LinksOnly
    if ($links.Count -gt 0) { throw ('a junction or a link below the root, refused before any write: ' + (@($links | ForEach-Object { $_.path }) -join ', ')) }
    $before = Test-IemDirectoryAcl -Path $Root -Rights $rights
    if ($before.Count -gt 0) { Set-IemDirectoryAcl -Path $Root -Rights $rights }
    $after = Test-IemDirectoryAcl -Path $Root -Rights $rights
    if ($after.Count -gt 0) { throw ('root ACL read-back: ' + ($after -join '; ')) }
    $reset = @()
    $differed = @()
    $items = Test-IemRootTree -Root $Root -Rights $rights
    foreach ($it in $items) {
        if ($it.link) { continue }
        # A folder reset before it may have brought it back already.
        if (-not (Test-Path -LiteralPath $it.path)) { continue }
        $bad = Test-IemInheritedItem -Path $it.path -Rights $rights
        if ($bad.Count -eq 0) { continue }
        Reset-IemInheritedItem -Root $Root -Rel $it.rel -Directory:$it.directory
        $reset += $it.rel
        $differed += $bad
    }
    $left = Test-IemRootTree -Root $Root -Rights $rights
    if ($left.Count -gt 0) { throw ('root ACL read-back below the root: ' + (@($left | ForEach-Object { $_.problems }) -join '; ')) }
    [pscustomobject]@{ root = $Root; user = $u.name; changed = ($before.Count -gt 0 -or $reset.Count -gt 0); before = $before; reset = $reset; differed = $differed }
}

function Test-IemReparsePoint {
    # Whether a file or folder is a junction or a link (the attributes of the
    # link itself, so a dangling one counts too). The elevated code never
    # follows one. $false when nothing is there.
    param([Parameter(Mandatory)][string]$Path)
    try { $a = [IO.File]::GetAttributes($Path) } catch {
        $e = $_.Exception
        while ($null -ne $e.InnerException) { $e = $e.InnerException }
        if ($e -is [IO.FileNotFoundException] -or $e -is [IO.DirectoryNotFoundException]) { return $false }
        throw
    }
    return (($a -band [IO.FileAttributes]::ReparsePoint) -ne 0)
}

function Test-IemElevatedItem {
    # A folder or file the Highest tasks run, load or write (design section 5.1):
    # no junction or link, owned by Administrators or SYSTEM, and a DACL that
    # grants exactly Get-IemElevatedRights (the user reads only): nothing
    # denied, nobody else. A folder carries its own protected rules (as
    # Set-IemDirectoryAcl writes them, inherited ones refused); a file inherits
    # its folder's, so a file's rules count inherited or explicit, and either
    # way they must be exactly those. Returns the differences.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$UserSid)
    if (Test-IemReparsePoint -Path $Path) { return ,@("$Path is a junction or a link") }
    if (-not (Test-Path -LiteralPath $Path)) { return ,@("$Path does not exist") }
    $acl = Get-Acl -LiteralPath $Path
    $bad = @()
    $owner = $acl.GetOwner([System.Security.Principal.SecurityIdentifier]).Value
    if (@($script:SidAdmins, $script:SidSystem) -notcontains $owner) { $bad += "$Path is owned by $owner" }
    $rights = Get-IemElevatedRights -UserSid $UserSid
    if (Test-Path -LiteralPath $Path -PathType Container) {
        $dirBad = Test-IemDirectoryAcl -Path $Path -Rights $rights
        foreach ($b in $dirBad) { $bad += ('{0}: {1}' -f $Path, $b) }
        return ,$bad
    }
    $seen = @{}
    foreach ($r in @($acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))) {
        $sid = $r.IdentityReference.Value
        if (-not $rights.ContainsKey($sid)) { $bad += "${Path}: a rule for $sid"; continue }
        if ("$($r.AccessControlType)" -ne 'Allow') { $bad += "${Path}: a deny rule for $sid"; continue }
        # An allow rule always carries Synchronize (1048576).
        if ([int]$r.FileSystemRights -ne (([int]$rights[$sid]) -bor 1048576)) { $bad += ('{0}: {1} {2}' -f $Path, $sid, $r.FileSystemRights) }
        if ($seen.ContainsKey($sid)) { $bad += "${Path}: two rules for $sid" }
        $seen[$sid] = $true
    }
    foreach ($sid in $rights.Keys) { if (-not $seen.ContainsKey($sid)) { $bad += "${Path}: no rule for $sid" } }
    return ,$bad
}

function New-IemElevatedSecurity {
    # The elevated root's folders: owner Administrators and a protected DACL
    # (Get-IemElevatedRights) that everything below inherits.
    param([Parameter(Mandatory)][string]$UserSid)
    $sec = New-Object System.Security.AccessControl.DirectorySecurity
    $sec.SetOwner((New-Object System.Security.Principal.SecurityIdentifier $script:SidAdmins))
    $sec.SetAccessRuleProtection($true, $false)
    $rights = Get-IemElevatedRights -UserSid $UserSid
    foreach ($sid in $rights.Keys) {
        $id = New-Object System.Security.Principal.SecurityIdentifier $sid
        $sec.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($id, $rights[$sid], 'ContainerInherit, ObjectInherit', 'None', 'Allow')))
    }
    return $sec
}

function Set-IemAdminsOwner {
    # A file's owner becomes Administrators (its DACL is left alone).
    param([Parameter(Mandatory)][string]$Path)
    if (Test-IemReparsePoint -Path $Path) { throw "$Path is a junction or a link: refused" }
    $fs = New-Object System.Security.AccessControl.FileSecurity
    $fs.SetOwner((New-Object System.Security.Principal.SecurityIdentifier $script:SidAdmins))
    [IO.File]::SetAccessControl($Path, $fs)
}

function Install-IemElevatedFolder {
    # One folder of the elevated root. A new one is created with its owner and
    # DACL in one step (no moment in which another user may add to it). One
    # that exists must be no junction or link and owned by Administrators or
    # SYSTEM, else it is refused (someone else made it: inspect it by hand);
    # its owner and DACL are then set again. Read back.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$UserSid)
    if (Test-IemReparsePoint -Path $Path) { throw "$Path is a junction or a link: refused" }
    $sec = New-IemElevatedSecurity -UserSid $UserSid
    if (Test-Path -LiteralPath $Path -PathType Container) {
        $owner = (Get-Acl -LiteralPath $Path).GetOwner([System.Security.Principal.SecurityIdentifier]).Value
        if (@($script:SidAdmins, $script:SidSystem) -notcontains $owner) {
            throw "$Path exists and is owned by $owner, not Administrators or SYSTEM: refused (inspect it and remove it by hand)"
        }
        [IO.Directory]::SetAccessControl($Path, $sec)
    } else {
        [void][IO.Directory]::CreateDirectory($Path, $sec)
    }
    $bad = Test-IemElevatedItem -Path $Path -UserSid $UserSid
    if ($bad.Count -gt 0) { throw ('elevated folder read-back: ' + ($bad -join '; ')) }
}

function Write-IemElevatedFile {
    # A file of the elevated root, written fresh (an old one is removed first,
    # so none of its rules survive; the new one inherits the folder's), owned
    # by Administrators.
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][byte[]]$Bytes)
    if (Test-IemReparsePoint -Path $Path) { throw "$Path is a junction or a link: refused" }
    if (Test-Path -LiteralPath $Path) { Remove-Item -LiteralPath $Path -Force }
    [IO.File]::WriteAllBytes($Path, $Bytes)
    Set-IemAdminsOwner -Path $Path
}

Export-ModuleMember -Function *-Iem*
