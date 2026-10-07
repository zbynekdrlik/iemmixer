"""PowerShell an elevated ssh session runs before it trusts anything in the
user's root (#15, the lane findings of 2026-10-07): admin-only folders under the
PC's elevated root (%ProgramData%\\iemmixer, the known folder as
Register-IemTasks resolves it), the bootstrap's stage, and the TEMP an Add-Type
compile uses. Composed on the dev box for iempc (bootstrap, tuning-install, the
refresh after activate: module_script; trace: iempc_trace.measure_load) and for
the S1c window tools (spike_window.measure_import). They run before IemPc.psm1
is loaded, so this is the one PowerShell copy of its Install-IemElevatedFolder
rule:

- a folder is created with its owner (Administrators) and protected DACL
  (Administrators and SYSTEM full, the session's user read and execute) in one
  step; one that exists must be no junction or link and owned by Administrators
  or SYSTEM (else refused: someone else made it) and gets both again; then it is
  read back: no junction or link, that owner, and no allow rule that lets anyone
  else change it;
- the stage: the uploaded module is read once into memory and checked by its
  sha256; those bytes are written fresh into <root>\\bootstrap-stage (a file
  link there is removed, never followed; a directory junction makes the delete
  throw), owned by Administrators, read back and checked again, and only that
  copy is imported. A process of the user may swap
  the upload after the check, never the staged copy, and the module's own
  $PSCommandPath (Install-IemElevatedDir copies it) is the staged copy too;
- TEMP and TMP point at <root>\\temp before IemTuning.psm1 loads: its Add-Type
  has csc write and then load a DLL in TEMP, and the session user's TEMP is open
  to every process of the user.

Each is one line (`powershell -Command -` reads stdin line by line) and keeps
no state between calls. The Windows CI runner runs them as composed: the stage
in Test-IemStage.ps1 (through iempc.module_script), TEMP in the asio-spike job's
analysis step (through tuning_window analysis-script)."""
from __future__ import annotations

import re

ROOT = "(Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) 'iemmixer')"
STAGE = "bootstrap-stage"
TEMP = "temp"
NAME = re.compile(r"[A-Za-z0-9_.-]+")
HEX64 = re.compile(r"[0-9a-f]{64}")
ADMINS = "(New-Object System.Security.Principal.SecurityIdentifier 'S-1-5-32-544')"
# An allow rule for anyone but Administrators and SYSTEM holding one of these
# bits lets them change the item: write data and append (add a file, a folder),
# write EA and attributes, delete a child, delete, write DAC and owner, generic
# write and all.
CHANGE = "0x500D0156"

# $iemOwn: no junction or link, owned by Administrators or SYSTEM. $iemOnly:
# that, and nobody else may change it. $iemDir: one admin-only folder.
HELPERS = " ; ".join([
    "$iemAdm = @('S-1-5-32-544', 'S-1-5-18')",
    "$iemOwn = { param([string]$p) if (([IO.File]::GetAttributes($p) -band [IO.FileAttributes]::ReparsePoint) -ne 0) "
    "{ throw \"$p is a junction or a link: refused\" } ; "
    "$o = (Get-Acl -LiteralPath $p).GetOwner([Security.Principal.SecurityIdentifier]).Value ; "
    "if ($iemAdm -notcontains $o) { throw \"$p is owned by $o, not Administrators or SYSTEM: refused\" } }",
    "$iemOnly = { param([string]$p) & $iemOwn $p ; "
    "foreach ($x in @((Get-Acl -LiteralPath $p).GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))) { "
    "if (\"$($x.AccessControlType)\" -eq 'Allow' -and $iemAdm -notcontains $x.IdentityReference.Value -and "
    f"([int]$x.FileSystemRights -band {CHANGE}) -ne 0) {{ throw \"$p may be changed by $($x.IdentityReference.Value): refused\" }} }} }}",
    "$iemDir = { param([string]$d) $s = New-Object System.Security.AccessControl.DirectorySecurity ; "
    f"$s.SetOwner({ADMINS}) ; $s.SetAccessRuleProtection($true, $false) ; "
    "foreach ($a in @(@('S-1-5-32-544', [Security.AccessControl.FileSystemRights]::FullControl), "
    "@('S-1-5-18', [Security.AccessControl.FileSystemRights]::FullControl), "
    "@([Security.Principal.WindowsIdentity]::GetCurrent().User.Value, [Security.AccessControl.FileSystemRights]::ReadAndExecute))) { "
    "$s.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule("
    "(New-Object System.Security.Principal.SecurityIdentifier $a[0]), $a[1], 'ContainerInherit, ObjectInherit', 'None', 'Allow'))) } ; "
    "[void][IO.Directory]::CreateDirectory($d, $s) ; & $iemOwn $d ; [IO.Directory]::SetAccessControl($d, $s) ; & $iemOnly $d }",
])


def folder(var: str, name: str, root: str) -> str:
    """`$<var>` = <root>\\<name>, the root and it admin-only (read back)."""
    return f"{HELPERS} ; $iemRoot = {root} ; ${var} = Join-Path $iemRoot '{name}' ; & $iemDir $iemRoot ; & $iemDir ${var}"


def staged_import(src: str, name: str, hexd: str, root: str = ROOT) -> str:
    """Statements that import the module uploaded at `src` (a PowerShell string)
    only from its admin-only stage copy `name`, checked by `hexd` before and
    after the copy. `root`: the elevated root (a PowerShell expression)."""
    if not NAME.fullmatch(name) or not HEX64.fullmatch(hexd):
        raise ValueError(f"stage: not a module name and a sha256: {name!r} {hexd!r}")
    return " ; ".join([
        f"$iemB = [IO.File]::ReadAllBytes({src})",
        "$iemX = [Security.Cryptography.SHA256]::Create() ; "
        "$iemH = -join @($iemX.ComputeHash($iemB) | ForEach-Object { $_.ToString('x2') }) ; $iemX.Dispose()",
        f"if ($iemH -cne '{hexd}') {{ throw ('sha256 mismatch: ' + {src}) }}",
        folder("iemStage", STAGE, root),
        f"$iemMod = Join-Path $iemStage '{name}' ; [IO.File]::Delete($iemMod) ; [IO.File]::WriteAllBytes($iemMod, $iemB)",
        f"$iemF = New-Object System.Security.AccessControl.FileSecurity ; $iemF.SetOwner({ADMINS}) ; "
        "[IO.File]::SetAccessControl($iemMod, $iemF) ; & $iemOnly $iemMod",
        f"if ((Get-FileHash -LiteralPath $iemMod -Algorithm SHA256).Hash.ToLowerInvariant() -cne '{hexd}') "
        "{ throw ('sha256 mismatch after the copy: ' + $iemMod) }",
        "Import-Module $iemMod -Force",
    ])


def temp_first(root: str = ROOT) -> str:
    """Statements that point TEMP and TMP at <root>\\temp (admin-only, read
    back): put them before any import of IemTuning.psm1 (its Add-Type)."""
    return f"{folder('iemTemp', TEMP, root)} ; $env:TEMP = $iemTemp ; $env:TMP = $iemTemp"
