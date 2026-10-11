#Requires -Version 5.1
# Fast Startup's facts (#35; split out of IemPc.psm1 by responsibility, #36):
# HiberbootEnabled and the hibernation file, their judgement
# (Get-IemFastStartup, pure) and the warning on the boot task's row
# (Get-IemBootTaskWarnings): with Fast Startup on, a shutdown fires no boot
# trigger at the next start.
# IemPc.psm1 imports this module from its own folder (-Global, as IemTuning
# imports IemTuningStore, #34), so it goes wherever IemPc.psm1 goes: the
# bundle, the bootstrap's admin-only stage, <elevated root>\tasks and the
# cutover's and the ssh shell undo's folders. Its functions reach the other
# IemPc modules' through the global session state. Nothing is ended by force
# (I8). No site value lives here (P6).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ---- Fast Startup (#35) ----

function Get-IemFastStartup {
    # Fast Startup as the next shutdown will act (pure; #35, review of PR
    # #40). It is on only when HiberbootEnabled is 1 AND the hibernation file
    # is present: a shutdown with it on hibernates the system session into
    # that file, and the next start fires no boot trigger, so the boot task
    # does not run. Without the file Windows shuts down fully whatever
    # HiberbootEnabled says (the venue PC: HiberbootEnabled 1, hibernation off
    # in powercfg /a). -Hiberboot: the registry value ($null: absent or
    # unreadable); -HiberFile: the hibernation file present
    # (Test-IemHiberFilePresent; $null: unreadable). active: $true when both hold, $false when either is known
    # off, $null when a fact is unknown and none is off. problem: the text
    # only when active is $true, else ''.
    param($Hiberboot = $null, $HiberFile = $null)
    $hb = $null
    if ($null -ne $Hiberboot) { $hb = [string]$Hiberboot }
    $file = $null
    if ($HiberFile -is [bool]) { $file = $HiberFile }
    $active = $null
    if (($null -ne $hb -and $hb -cne '1') -or ($file -eq $false)) {
        $active = $false
    } elseif ($hb -ceq '1' -and $file -eq $true) {
        $active = $true
    }
    $problem = ''
    if ($active -eq $true) {
        $problem = 'Fast Startup is on (HiberbootEnabled 1, the hibernation file present): a shutdown hibernates ' +
            'the system session and the next start fires no boot trigger, so iemmixer-boot-pref does not run and ' +
            'the logon task is the first to restore the preference (a restart is a full boot). Turn Fast Startup ' +
            'off (Power Options, or hibernation off) to keep the boot task first.'
    }
    [pscustomobject]@{ hiberboot_enabled = $Hiberboot; hiberfile_present = $file; active = $active; problem = $problem }
}

function Test-IemHiberFilePresent {
    # The hibernation file Fast Startup needs: hiberfil.sys at the root of
    # the system volume, where Windows keeps it while hibernation is on
    # (powercfg /h off deletes it), whatever the registry's setting says. It
    # is found by listing the root (FindFirstFile), never opened: the kernel
    # holds it open. Nothing is compiled (no Add-Type: in Windows PowerShell
    # 5.1 that builds in the user's temp folder, which a process of the user
    # could change under an elevated session).
    $root = [IO.Path]::GetPathRoot([Environment]::SystemDirectory)
    if (-not $root) { throw 'the system folder has no root' }
    $found = @([IO.Directory]::GetFiles($root, 'hiberfil.sys') | Where-Object { [IO.Path]::GetFileName($_) -ieq 'hiberfil.sys' })
    return ($found.Count -gt 0)
}

function Get-IemFastStartupState {
    # Get-IemFastStartup over this PC's facts: HiberbootEnabled
    # (HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Power) and
    # Test-IemHiberFilePresent. A read that fails leaves its fact $null and
    # its message in error, never a guess.
    $hb = $null
    $file = $null
    $errors = @()
    try {
        $hb = (Get-Item -LiteralPath 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Power').GetValue('HiberbootEnabled', $null)
    } catch { $errors += ('HiberbootEnabled: ' + $_.Exception.Message) }
    try { $file = Test-IemHiberFilePresent } catch { $errors += ('the hibernation file: ' + $_.Exception.Message) }
    $s = Get-IemFastStartup -Hiberboot $hb -HiberFile $file
    [pscustomobject]@{ hiberboot_enabled = $s.hiberboot_enabled; hiberfile_present = $s.hiberfile_present
                       active = $s.active; problem = $s.problem; error = ($errors -join '; ') }
}

function Get-IemBootTaskWarnings {
    # What Register-IemTasks says about the boot task without refusing it
    # (pure): Fast Startup that is on (Get-IemFastStartup) skips its trigger
    # at every start after a shutdown; one it could not judge (a fact
    # unread, Get-IemFastStartupState's error) is named as such, never as on.
    param([Parameter(Mandatory)]$FastStartup)
    if ($FastStartup.active -eq $true) { return ,@([string]$FastStartup.problem) }
    $err = [string](Get-IemProp -Object $FastStartup -Name 'error')
    if ($null -eq $FastStartup.active) {
        # Unknown is named, never read as off (review of PR #41): a read error, or a
        # fact absent without one (HiberbootEnabled missing, the file not read).
        $unknown = @()
        if ($null -eq $FastStartup.hiberboot_enabled -and $err -cnotlike '*HiberbootEnabled*') { $unknown += 'HiberbootEnabled absent' }
        if ($null -eq $FastStartup.hiberfile_present -and $err -cnotlike '*hibernation file*') { $unknown += 'the hibernation file not read' }
        if ($err) { $unknown += $err }
        $why = $unknown -join ', '
        return ,@(('Fast Startup could not be judged ({0}): if it is on, a shutdown skips iemmixer-boot-pref' -f $why))
    }
    return ,@()
}

Export-ModuleMember -Function *-Iem*
