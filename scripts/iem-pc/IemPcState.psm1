#Requires -Version 5.1
# The bootstrap's read-only facts (design note section 6, plan Task 16
# Step 1; split out of IemPc.psm1 by responsibility, #36): the tunnel's
# origin, the predecessor's task and app, the boot task's log and
# Get-IemBootstrapState. They only read.
# IemPc.psm1 imports this module from its own folder (-Global, as IemTuning
# imports IemTuningStore, #34), so it goes wherever IemPc.psm1 goes: the
# bundle, the bootstrap's admin-only stage, <elevated root>\tasks and the
# cutover's and the ssh shell undo's folders. Its functions reach the other
# IemPc modules' through the global session state. Nothing is ended by force
# (I8). No site value lives here (P6).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:TaskNames = @('iemmixer-guard', 'iemmixer-StartREAPER', 'iemmixer-StartApp', 'iemmixer-probe',
                      'iemmixer-tuning', 'iemmixer-exclude', 'iemmixer-logon', 'iemmixer-boot-pref')
# The boot task's log in <elevated root>\tasks\out (#35).
$script:BootPrefLog = 'boot-pref.log'

# ---- read-only facts (design section 6, plan Task 16 Step 1) ----

function ConvertFrom-IemTunnelConfig {
    # The connector's /config (the remotely managed ingress): each rule's
    # hostname and origin. The summary names no host (it may go on #9).
    param([Parameter(Mandatory)]$Config, [string]$Metrics = '')
    $rules = @()
    foreach ($r in @(Get-IemProp (Get-IemProp $Config 'config') 'ingress')) {
        if ($null -eq $r) { continue }
        $service = [string](Get-IemProp $r 'service')
        $row = [ordered]@{ hostname = [string](Get-IemProp $r 'hostname'); service = $service; origin = $false
                           scheme = ''; host = ''; port = 0; loopback = $false }
        if ($service -match '^(https?|wss?|tcp)://') {
            $uri = New-Object System.Uri $service
            $row.origin = $true
            $row.scheme = $uri.Scheme
            $row.host = $uri.Host
            $row.port = $uri.Port
            $row.loopback = ($uri.IsLoopback -or $uri.Host -eq 'localhost')
        }
        $rules += [pscustomobject]$row
    }
    $origins = @($rules | Where-Object { $_.origin })
    $allLoopback = ($origins.Count -gt 0) -and (@($origins | Where-Object { -not $_.loopback }).Count -eq 0)
    $parts = @($origins | ForEach-Object {
        $where = 'remote'
        if ($_.loopback) { $where = 'loopback' }
        '{0} :{1} {2}' -f $_.scheme, $_.port, $where
    })
    [pscustomobject]@{
        metrics = $Metrics
        version = (Get-IemProp $Config 'version')
        rules = $rules
        all_loopback = $allLoopback
        summary = ('{0} rule(s); origins: {1}' -f $rules.Count, ($parts -join ', '))
    }
}

function Get-IemTunnelOrigin {
    # Read-only (G7): the tunnel's origin as the connector reports it on its
    # metrics server (/config). The ingress is never edited.
    param([string]$Metrics = '', [int[]]$Ports = @(20241, 20242, 20243, 20244, 20245))
    $bases = @($Ports | ForEach-Object { 'http://127.0.0.1:{0}' -f $_ })
    if ($Metrics) { $bases = @($Metrics.TrimEnd('/')) }
    $errors = @()
    foreach ($b in $bases) {
        try { $resp = Invoke-WebRequest -UseBasicParsing -Uri ($b + '/config') -TimeoutSec 5 }
        catch { $errors += ('{0}: {1}' -f $b, $_.Exception.Message); continue }
        return (ConvertFrom-IemTunnelConfig -Config ($resp.Content | ConvertFrom-Json) -Metrics $b)
    }
    throw ('no tunnel connector answered /config: ' + ($errors -join '; '))
}

function Get-IemTriggerKind {
    param([Parameter(Mandatory)][int]$Type)
    $names = @{ 0 = 'event'; 1 = 'time'; 2 = 'daily'; 3 = 'weekly'; 4 = 'monthly'; 5 = 'monthly-dow'; 6 = 'idle'
                7 = 'registration'; 8 = 'boot'; 9 = 'logon'; 11 = 'session-state' }
    if ($names.ContainsKey($Type)) { return $names[$Type] }
    return ('type {0}' -f $Type)
}

function Test-IemTriggersMayFire {
    # Can the task start REAPER in dev time? Boot and logon triggers fire only at
    # a boot or a logon (the PC then comes back in event mode); every other
    # enabled trigger can fire at any time.
    param([Parameter(Mandatory)][bool]$Enabled, [object[]]$Triggers = @())
    if (-not $Enabled) { return $false }
    return (@($Triggers | Where-Object { $_.enabled -and @(8, 9) -notcontains $_.type }).Count -gt 0)
}

function Get-IemPredecessorFacts {
    # Read-only: the predecessor's REAPER task (its triggers: can one fire in
    # dev time?) and the app's version resource and exe SHA-256 ([guard]
    # app_exe_sha256). The output holds site values: only non-site parts go on #9.
    param([Parameter(Mandatory)][string]$ReaperTask, [Parameter(Mandatory)][string]$AppExe)
    $i = $ReaperTask.LastIndexOf('\')
    if ($i -lt 0) { throw "task path '$ReaperTask' needs its folder (\folder\name)" }
    $folderPath = $ReaperTask.Substring(0, $i)
    if (-not $folderPath) { $folderPath = '\' }
    $sch = Connect-IemScheduler
    $task = Get-IemRegisteredTask -Scheduler $sch -Folder $folderPath -Name $ReaperTask.Substring($i + 1)
    if ($null -eq $task) { throw "task $ReaperTask does not exist" }
    $def = $task.Definition
    $triggers = @()
    foreach ($t in $def.Triggers) {
        $type = [int]$t.Type
        $triggers += [pscustomobject]@{ type = $type; kind = (Get-IemTriggerKind -Type $type); enabled = [bool]$t.Enabled
                                        start = [string]$t.StartBoundary; end = [string]$t.EndBoundary
                                        repeat = [string]$t.Repetition.Interval }
    }
    $enabled = [bool]$def.Settings.Enabled
    $xml = [xml]$def.XmlText
    $node = $xml.DocumentElement.SelectSingleNode("*[local-name()='Triggers']")
    $triggersXml = ''
    if ($null -ne $node) { $triggersXml = $node.OuterXml }
    $vi = (Get-Item -LiteralPath $AppExe).VersionInfo
    [pscustomobject]@{
        reaper_task = [pscustomobject]@{ enabled = $enabled; state = [int]$task.State; triggers = $triggers
                                         may_fire_in_dev_time = (Test-IemTriggersMayFire -Enabled $enabled -Triggers $triggers)
                                         xml = $triggersXml }
        app = [pscustomobject]@{ file_version = [string]$vi.FileVersion; product_version = [string]$vi.ProductVersion
                                 sha256 = (Get-FileHash -LiteralPath $AppExe -Algorithm SHA256).Hash.ToLowerInvariant() }
    }
}

function Format-IemTaskTime {
    # A task's last run time (local, as Task Scheduler gives it) in UTC, ISO
    # 8601; '' when it never ran (Task Scheduler then gives a time before 2000).
    param($Time)
    if ($null -eq $Time) { return '' }
    $t = [datetime]$Time
    if ($t.Year -lt 2000) { return '' }
    return $t.ToUniversalTime().ToString('o')
}

function Get-IemBootPrefLog {
    # The boot task's last logged run (#35): the lines of <elevated root>\
    # tasks\out\boot-pref.log from its last "boot-pref " header on, empty
    # lines dropped (the header, then reg.exe's read-back: the key and the
    # value, or its error). @() before the first run.
    param([Parameter(Mandatory)][string]$Path)
    if (Test-IemReparsePoint -Path $Path) { throw "$Path is a junction or a link: refused" }
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return ,@() }
    $lines = @([IO.File]::ReadAllLines($Path) | Where-Object { $_.Trim() })
    $start = -1
    for ($i = 0; $i -lt $lines.Count; $i++) { if ($lines[$i].StartsWith('boot-pref ')) { $start = $i } }
    if ($start -lt 0) { return ,@() }
    return ,@($lines[$start..($lines.Count - 1)])
}

function Get-IemBootstrapState {
    # Read-only (plan Task 16 Step 1): REAPER and the app running, the driver
    # module's holders, the preference, our tasks (descriptor, last result,
    # last run in UTC), the boot task's last logged run (#35; its log in
    # -ElevatedRoot, default %ProgramData%\iemmixer), Fast Startup
    # (Get-IemFastStartupState: HiberbootEnabled, the hibernation file, on or
    # not and its problem; with it on, a shutdown hibernates the system
    # session and the next start fires no boot trigger), the root's DACL and
    # every item below it, the firewall rule, the network categories,
    # Defender. The result holds site values (the user, the preference key in
    # the boot log): it stays on the dev box, never on a public ticket (P6).
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$Module, [Parameter(Mandatory)][string]$PrefKey,
          [Parameter(Mandatory)][string]$PrefName, [Parameter(Mandatory)][string]$AppImage, [string]$ReaperImage = 'reaper',
          [string]$Folder = '\iemmixer', [string]$FirewallRule = 'iemmixer-http', [string]$User = '', [string]$ElevatedRoot = '')
    $ElevatedRoot = Resolve-IemElevatedRoot -ElevatedRoot $ElevatedRoot
    $u = Resolve-IemUser -User $User
    $sch = Connect-IemScheduler
    $tasks = @()
    foreach ($name in $script:TaskNames) {
        $t = Get-IemRegisteredTask -Scheduler $sch -Folder $Folder -Name $name
        if ($null -eq $t) { $tasks += [pscustomobject]@{ task = $name; exists = $false; state = 0; last_result = 0; last_run = ''; sddl_ok = $false }; continue }
        $sddl = [string]$t.GetSecurityDescriptor(4)
        $tasks += [pscustomobject]@{ task = $name; exists = $true; state = [int]$t.State; last_result = [int64]$t.LastTaskResult
                                     last_run = (Format-IemTaskTime -Time $t.LastRunTime); sddl_ok = (Test-IemTaskSddl -Sddl $sddl -UserSid $u.sid) }
    }
    $bootLog = [IO.Path]::Combine($ElevatedRoot, 'tasks', 'out', $script:BootPrefLog)
    $bootLines = @()
    $bootError = ''
    try { $bootLines = Get-IemBootPrefLog -Path $bootLog } catch { $bootError = $_.Exception.Message }
    $fastStartup = Get-IemFastStartupState
    $pref = $null
    $prefError = ''
    try { $pref = Get-IemPref -Key $PrefKey -Name $PrefName } catch { $prefError = $_.Exception.Message }
    $rootExists = Test-Path -LiteralPath $Root -PathType Container
    $rootBad = @()
    if ($rootExists) {
        if (Test-IemReparsePoint -Path $Root) {
            $rootBad = @("$Root is a junction or a link")
        } else {
            # The root's DACL and every item below it (what Set-IemRootAcl sets).
            $rootRights = Get-IemRootRights -UserSid $u.sid
            $rootBad = Test-IemDirectoryAcl -Path $Root -Rights $rootRights
            $below = Test-IemRootTree -Root $Root -Rights $rootRights
            $rootBad = @($rootBad) + @($below | ForEach-Object { $_.problems })
        }
    }
    $fw = Get-IemFirewallRule -Name $FirewallRule
    $defender = 'available'
    try { $null = Get-MpComputerStatus } catch { $defender = 'unavailable: ' + $_.Exception.Message }
    $app = $AppImage -replace '\.exe$', ''
    [pscustomobject]@{
        user = $u.name
        reaper = @(Get-Process -Name $ReaperImage -ErrorAction SilentlyContinue).Count
        app = @(Get-Process -Name $app -ErrorAction SilentlyContinue).Count
        holders = (Get-IemModuleHolders -Module $Module)
        pref = $pref
        pref_error = $prefError
        tasks = $tasks
        boot_pref = [pscustomobject]@{ path = $bootLog; log = $bootLines; log_error = $bootError }
        fast_startup = $fastStartup
        root = [pscustomobject]@{ exists = $rootExists; acl_ok = ($rootExists -and $rootBad.Count -eq 0); problems = $rootBad }
        firewall = [pscustomobject]@{ present = ($null -ne $fw); ok = (Test-IemFirewallRule -Rule $fw -Enabled 'True'); rule = $fw }
        networks = @(Get-NetConnectionProfile -ErrorAction SilentlyContinue | ForEach-Object { "$($_.NetworkCategory)" })
        defender = $defender
    }
}

Export-ModuleMember -Function *-Iem*
