"""PowerShell an elevated ssh session runs before it trusts anything in the
user's root (#15, the lane findings of 2026-10-07): admin-only folders under the
PC's elevated root (%ProgramData%\\iemmixer, the known folder as
Register-IemTasks resolves it), the bootstrap's stage, and the TEMP an Add-Type
compile uses. Composed on the dev box for iempc (bootstrap, tuning-install, the
refresh after activate: module_script; trace: iempc_trace.measure_load) and for
the S1a/S1c/golden window sessions (spike_window.ps_script, measure_import,
trace_stop_import; golden_window.ps_script). They run before IemPc.psm1 is
loaded, so this is the one PowerShell copy of its Install-IemElevatedFolder
rule:

- a folder is created with its owner (Administrators) and protected DACL
  (Administrators and SYSTEM full, the session's user read and execute) in one
  step; one that exists must be no junction or link and owned by Administrators
  or SYSTEM (else refused: someone else made it) and gets both again; then it is
  read back: no junction or link, that owner, and no allow rule that lets anyone
  else change it;
- the stage: the uploaded module is read once into memory and checked by its
  sha256; those bytes are written into <root>\\bootstrap-stage (a file link
  there is removed, never followed; a directory junction makes the delete
  throw), owned by Administrators, read back and checked again, and only that
  copy is imported. A stage copy that already holds exactly those bytes (a
  file, no link) is kept, never rewritten: window sessions run at the same
  time (a run's poll and a preempt), and a rewrite under another session's
  import would fail it or hand it half a file. A process of the user may swap
  the upload after the check, never the staged copy, and the module's own
  $PSCommandPath (Install-IemElevatedDir copies it) is the staged copy too. A
  window session checks each module against `$iemSums` (sums_table: the
  attested bundle record's sums, composed on the dev box), and a module that
  loads another from its own folder (SpikePc GoldenPc, IemMeasure IemTuning)
  has that one staged first;
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


def _want(name: str, hexd: str | None) -> str:
    """The expected sha256 of module `name` as PowerShell: the literal `hexd`,
    or (None) the window session's `$iemSums` entry."""
    if not NAME.fullmatch(name) or not (hexd is None or HEX64.fullmatch(hexd)):
        raise ValueError(f"stage: not a module name and a sha256: {name!r} {hexd!r}")
    return f"$iemSums['{name}']" if hexd is None else f"'{hexd}'"


def _hash(path: str) -> str:
    return f"(Get-FileHash -LiteralPath {path} -Algorithm SHA256).Hash.ToLowerInvariant()"


def _same(path: str, want: str) -> str:
    """True when `path` is a file, no junction or link, holding exactly `want`."""
    return (f"([IO.File]::Exists({path}) -and (([IO.File]::GetAttributes({path}) -band [IO.FileAttributes]::ReparsePoint) -eq 0) "
            f"-and ({_hash(path)} -ceq {want}))")


def _owned(path: str) -> str:
    return (f"$iemF = New-Object System.Security.AccessControl.FileSecurity ; $iemF.SetOwner({ADMINS}) ; "
            f"[IO.File]::SetAccessControl({path}, $iemF)")


def staged(mods: list[tuple[str, str, str | None]], root: str = ROOT) -> str:
    """Statements that put each (src, name, hexd) into the admin-only stage:
    the module at `src` (a PowerShell string) read once and checked by `hexd`
    (None: `$iemSums[name]`), the stage made and read back, the copy written
    unless the stage already holds exactly those bytes, then read back
    admin-only and checked again. `$iemMod` is the last one's stage copy."""
    parts = []
    for i, (src, name, hexd) in enumerate(mods):
        want = _want(name, hexd)
        parts += [
            f"$iemB = [IO.File]::ReadAllBytes({src})",
            "$iemX = [Security.Cryptography.SHA256]::Create() ; "
            "$iemH = -join @($iemX.ComputeHash($iemB) | ForEach-Object { $_.ToString('x2') }) ; $iemX.Dispose()",
            f"if ($iemH -cne {want}) {{ throw ('sha256 mismatch: ' + {src}) }}",
            *([folder("iemStage", STAGE, root)] if i == 0 else []),
            f"$iemMod = Join-Path $iemStage '{name}'",
            f"if (-not {_same('$iemMod', want)}) {{ [IO.File]::Delete($iemMod) ; [IO.File]::WriteAllBytes($iemMod, $iemB) ; "
            f"{_owned('$iemMod')} }}",
            "& $iemOnly $iemMod",
            f"if ({_hash('$iemMod')} -cne {want}) {{ throw ('sha256 mismatch after the copy: ' + $iemMod) }}",
        ]
    return " ; ".join(parts)


def staged_import(src: str, name: str, hexd: str, root: str = ROOT) -> str:
    """Statements that import the module uploaded at `src` (a PowerShell string)
    only from its admin-only stage copy `name`, checked by `hexd` before and
    after the copy. `root`: the elevated root (a PowerShell expression)."""
    return f"{staged([(src, name, hexd)], root)} ; Import-Module $iemMod -Force"


def sums_table(sums: dict[str, str]) -> str:
    """`$iemSums`, the attested sha256 of each module a window session may
    stage (staged with hexd None), from the bundle record on the dev box."""
    for name, hexd in sums.items():
        _want(name, hexd)
    return "$iemSums = @{ " + "; ".join(f"'{n}' = '{h}'" for n, h in sorted(sums.items())) + " }"


def import_staged(name: str, opts: str = "-Force") -> str:
    """Imports the stage copy `name` that staged() put there in this session."""
    _want(name, None)
    return f"Import-Module (Join-Path $iemStage '{name}') {opts}"


def temp_first(root: str = ROOT) -> str:
    """Statements that point TEMP and TMP at <root>\\temp (admin-only, read
    back): put them before any import of IemTuning.psm1 (its Add-Type)."""
    return f"{folder('iemTemp', TEMP, root)} ; $env:TEMP = $iemTemp ; $env:TMP = $iemTemp"
