#Requires -Version 5.1
# S1b golden renders: PC-side work (design note §4). Never ends a process by
# force; the render instance never uses audio mode 3 (ASIO).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:AllowedFxHeads = @(
    'VST "VST: ReaEQ (Cockos)" reaeq.dll 0 "" 1919247729<56535472656571726561657100000000> ""',
    'JS utility/volume_pan ""',
    'JS loser/MGA_JSLimiterST ""'
)
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding $false
# Functions that `return ,$array` hand back exactly one array: assign their
# result (`$x = Get-GoldenAsioHolders …`); `@(Get-Golden…)` would nest it.

function Invoke-GoldenExe {
    param([Parameter(Mandatory)][string]$FilePath, [Parameter(Mandatory)][string[]]$ArgumentList)
    $out = [IO.Path]::GetTempFileName(); $err = [IO.Path]::GetTempFileName()
    try {
        $p = Start-Process -FilePath $FilePath -ArgumentList $ArgumentList -NoNewWindow -Wait -PassThru -RedirectStandardOutput $out -RedirectStandardError $err
        return $p.ExitCode
    } finally { Remove-Item -LiteralPath $out, $err -ErrorAction SilentlyContinue }
}

function Get-GoldenManifest {
    # Volatile roots belong to a program that keeps running during the window
    # (the predecessor app): a file it holds open is listed as 'unreadable'.
    param([Parameter(Mandatory)][hashtable]$Roots, [string[]]$Volatile = @())
    $rows = New-Object System.Collections.Generic.List[object]
    foreach ($name in ($Roots.Keys | Sort-Object)) {
        $root = [IO.Path]::GetFullPath($Roots[$name]).TrimEnd('\')
        if (-not (Test-Path -LiteralPath $root -PathType Container)) { throw "root '$name' is missing: $root" }
        foreach ($f in (Get-ChildItem -LiteralPath $root -Recurse -File -Force | Sort-Object FullName)) {
            $sha = $null
            try { $sha = (Get-FileHash -LiteralPath $f.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
            catch { if ($Volatile -notcontains $name) { throw }; $sha = 'unreadable' }
            $rows.Add([pscustomobject]@{
                root   = $name
                rel    = $f.FullName.Substring($root.Length + 1)
                size   = $f.Length
                sha256 = $sha
                mtime  = $f.LastWriteTimeUtc.ToString('o')
            })
        }
    }
    return ,$rows.ToArray()
}

function Compare-GoldenManifest {
    param([Parameter(Mandatory)][AllowEmptyCollection()][object[]]$Before, [Parameter(Mandatory)][AllowEmptyCollection()][object[]]$After)
    $b = @{}; foreach ($r in $Before) { $b["$($r.root)|$($r.rel)"] = $r }
    $a = @{}; foreach ($r in $After) { $a["$($r.root)|$($r.rel)"] = $r }
    $changed = @(); $missing = @(); $extra = @(); $touched = @()
    foreach ($k in $b.Keys) {
        if (-not $a.ContainsKey($k)) { $missing += $k }
        elseif ($a[$k].sha256 -ne $b[$k].sha256 -or $a[$k].size -ne $b[$k].size) { $changed += $k }
        elseif ($a[$k].mtime -ne $b[$k].mtime) { $touched += $k }
    }
    foreach ($k in $a.Keys) { if (-not $b.ContainsKey($k)) { $extra += $k } }
    [pscustomobject]@{
        identical = (($changed.Count + $missing.Count + $extra.Count) -eq 0)
        changed = @($changed | Sort-Object); missing = @($missing | Sort-Object)
        extra = @($extra | Sort-Object); touched = @($touched | Sort-Object)
    }
}

function Select-GoldenRows {
    param([AllowEmptyCollection()][object[]]$Rows = @(), [string[]]$Volatile = @(), [switch]$OnlyVolatile)
    return ,@($Rows | Where-Object { ($Volatile -contains $_.root) -eq [bool]$OnlyVolatile })
}

function Get-GoldenTrees {
    param([Parameter(Mandatory)][string]$Path)
    $json = Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
    $roots = @{}
    foreach ($p in $json.PSObject.Properties) { $roots[$p.Name] = [Environment]::ExpandEnvironmentVariables([string]$p.Value) }
    return $roots
}

function Export-GoldenRegistry {
    param([Parameter(Mandatory)][string[]]$Keys, [Parameter(Mandatory)][string]$Dest)
    $files = @()
    for ($i = 0; $i -lt $Keys.Count; $i++) {
        $file = Join-Path $Dest "reg-$i.reg"
        $code = Invoke-GoldenExe -FilePath 'reg.exe' -ArgumentList @('export', "`"$($Keys[$i])`"", "`"$file`"", '/y')
        if ($code -ne 0) { throw "reg export failed for key $i (exit $code)" }
        $files += [pscustomobject]@{ key = $Keys[$i]; file = "reg-$i.reg"; sha256 = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant() }
    }
    return ,$files
}

function Invoke-GoldenBackup {
    param([Parameter(Mandatory)][hashtable]$Roots, [Parameter(Mandatory)][string]$Dest,
          [string[]]$RegistryKeys = @(), [string[]]$TaskNames = @(), [string[]]$VolatileRoots = @())
    if (Test-Path -LiteralPath $Dest) { throw "backup destination exists: $Dest" }
    foreach ($v in $VolatileRoots) { if (-not $Roots.ContainsKey($v)) { throw "volatile root '$v' is not a root" } }
    New-Item -ItemType Directory -Path $Dest | Out-Null
    $source = Get-GoldenManifest -Roots $Roots -Volatile $VolatileRoots
    $copies = @{}
    $robocopy = @{}
    foreach ($name in $Roots.Keys) {
        $target = Join-Path $Dest ("trees\" + $name)
        $code = Invoke-GoldenExe -FilePath 'robocopy.exe' -ArgumentList @("`"$($Roots[$name])`"", "`"$target`"", '/E', '/COPY:DAT', '/DCOPY:DAT', '/R:0', '/W:0', '/NP', '/NFL', '/NDL', '/NJH', '/NJS')
        if ($code -ge 8 -and $VolatileRoots -notcontains $name) { throw "robocopy failed for '$name' (exit $code)" }
        $robocopy[$name] = $code
        $copies[$name] = $target
    }
    $copy = Get-GoldenManifest -Roots $copies -Volatile $VolatileRoots
    $diff = Compare-GoldenManifest -Before (Select-GoldenRows -Rows $source -Volatile $VolatileRoots) -After (Select-GoldenRows -Rows $copy -Volatile $VolatileRoots)
    if (-not $diff.identical) { throw "backup copy differs from its source: $($diff.changed.Count) changed, $($diff.missing.Count) missing, $($diff.extra.Count) extra" }
    # Best effort for volatile roots: files held open or changed during the copy are counted, never fatal.
    $vdiff = Compare-GoldenManifest -Before (Select-GoldenRows -Rows $source -Volatile $VolatileRoots -OnlyVolatile) -After (Select-GoldenRows -Rows $copy -Volatile $VolatileRoots -OnlyVolatile)
    $unreadable = @($source | Where-Object { $_.sha256 -eq 'unreadable' }).Count
    $registry = @()   # never `$x = if … { @() }`: an empty array assigned that way becomes $null
    if ($RegistryKeys.Count -gt 0) { $registry = Export-GoldenRegistry -Keys $RegistryKeys -Dest $Dest }
    $tasks = @()
    for ($i = 0; $i -lt $TaskNames.Count; $i++) {
        $xml = Export-ScheduledTask -TaskName $TaskNames[$i]
        [IO.File]::WriteAllText((Join-Path $Dest "task-$i.xml"), $xml, $script:Utf8NoBom)
        $tasks += [pscustomobject]@{ name = $TaskNames[$i]; file = "task-$i.xml" }
    }
    $manifest = [pscustomobject]@{ schema = 1; created = (Get-Date).ToUniversalTime().ToString('o'); roots = $Roots; volatile = @($VolatileRoots); files = $source; registry = $registry; tasks = $tasks; robocopy = $robocopy }
    [IO.File]::WriteAllText((Join-Path $Dest 'manifest.json'), ($manifest | ConvertTo-Json -Depth 6), $script:Utf8NoBom)
    [pscustomobject]@{ files = $source.Count; bytes = ($source | Measure-Object size -Sum).Sum; registry = $registry.Count; tasks = $tasks.Count
        volatile = [pscustomobject]@{ roots = @($VolatileRoots); unreadable = $unreadable; copy_differs = ($vdiff.changed.Count + $vdiff.missing.Count + $vdiff.extra.Count) } }
}

function Compare-GoldenRegistry {
    param([Parameter(Mandatory)][string]$Backup, [Parameter(Mandatory)]$Manifest)
    $diffs = @()
    $now = Join-Path $Backup ('verify-' + (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssfff'))
    New-Item -ItemType Directory -Force -Path $now | Out-Null
    $saved = @($Manifest.registry | Where-Object { $_ })
    if ($saved.Count -gt 0) {
        $current = Export-GoldenRegistry -Keys @($saved | ForEach-Object { $_.key }) -Dest $now
        for ($i = 0; $i -lt $saved.Count; $i++) { if ($current[$i].sha256 -ne $saved[$i].sha256) { $diffs += $saved[$i].key } }
    }
    foreach ($t in @($Manifest.tasks | Where-Object { $_ })) {
        $xml = Export-ScheduledTask -TaskName $t.name
        if ($xml -ne [IO.File]::ReadAllText((Join-Path $Backup $t.file))) { $diffs += "task:$($t.name)" }
    }
    [pscustomobject]@{ identical = ($diffs.Count -eq 0); differences = $diffs }
}

function Invoke-GoldenVerify {
    param([Parameter(Mandatory)][string]$Backup, [switch]$Restore)
    $m = Get-Content -LiteralPath (Join-Path $Backup 'manifest.json') -Raw | ConvertFrom-Json
    $roots = @{}; foreach ($p in $m.roots.PSObject.Properties) { $roots[$p.Name] = [string]$p.Value }
    $vol = @(); if ($m.PSObject.Properties['volatile']) { $vol = @($m.volatile | Where-Object { $_ }) }
    $before = Select-GoldenRows -Rows @($m.files) -Volatile $vol
    $nowAll = Get-GoldenManifest -Roots $roots -Volatile $vol
    $diff = Compare-GoldenManifest -Before $before -After (Select-GoldenRows -Rows $nowAll -Volatile $vol)
    # Volatile roots are reported, never restored: their owner keeps running.
    $vdiff = Compare-GoldenManifest -Before (Select-GoldenRows -Rows @($m.files) -Volatile $vol -OnlyVolatile) -After (Select-GoldenRows -Rows $nowAll -Volatile $vol -OnlyVolatile)
    $restored = @(); $quarantined = @()
    if ($Restore -and -not $diff.identical) {
        foreach ($k in @($diff.changed + $diff.missing)) {
            $root, $rel = $k.Split([char[]]'|', 2)
            $dst = Join-Path $roots[$root] $rel
            New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dst) | Out-Null
            Copy-Item -LiteralPath (Join-Path (Join-Path $Backup "trees\$root") $rel) -Destination $dst -Force
            $restored += $k
        }
        foreach ($k in $diff.extra) {
            $root, $rel = $k.Split([char[]]'|', 2)
            $dst = Join-Path (Join-Path $Backup "quarantine\$root") $rel
            New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dst) | Out-Null
            Move-Item -LiteralPath (Join-Path $roots[$root] $rel) -Destination $dst
            $quarantined += $k
        }
        $diff = Compare-GoldenManifest -Before $before -After (Select-GoldenRows -Rows (Get-GoldenManifest -Roots $roots -Volatile $vol) -Volatile $vol)
    }
    $reg = Compare-GoldenRegistry -Backup $Backup -Manifest $m
    $volatile = [pscustomobject]@{ roots = $vol; changed = $vdiff.changed.Count; missing = $vdiff.missing.Count; extra = $vdiff.extra.Count; touched = $vdiff.touched.Count }
    [pscustomobject]@{ identical = ($diff.identical -and $reg.identical); files = $diff; registry = $reg; restored = $restored; quarantined = $quarantined; volatile = $volatile }
}

function Test-GoldenRpp {
    param([Parameter(Mandatory)][string]$Text)
    foreach ($line in ($Text -split "`r?`n")) {
        $t = $line.Trim()
        if ($t -match '^<(VST3?|JS|CLAP|AUi?|DX|LV2|VIDEO_EFFECT)\b' -and $script:AllowedFxHeads -notcontains $t.Substring(1)) { throw "plug-in not on the allowlist: $t" }
        if ($t.StartsWith('FILE ') -and $t -notmatch '^FILE "@@JOB@@\\stimuli\\[A-Za-z0-9._-]+\.wav"$') { throw "media path outside the job: $t" }
        if ($t.StartsWith('RENDER_FILE ') -and $t -notmatch '^RENDER_FILE "@@OUT@@\\[A-Za-z0-9._-]+"$') { throw "render target outside the job: $t" }
    }
}

function Invoke-GoldenStage {
    param([Parameter(Mandatory)][string]$Bundle, [Parameter(Mandatory)][string]$Job)
    $m = Get-Content -LiteralPath (Join-Path $Bundle 'bundle.json') -Raw | ConvertFrom-Json
    $listed = @{}
    foreach ($f in $m.files) {
        $p = Join-Path $Bundle ($f.path -replace '/', '\')
        if (-not (Test-Path -LiteralPath $p -PathType Leaf)) { throw "bundle file missing: $($f.path)" }
        if ((Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash.ToLowerInvariant() -ne $f.sha256) { throw "bundle file hash mismatch: $($f.path)" }
        $listed[[IO.Path]::GetFullPath($p)] = $true
    }
    foreach ($f in (Get-ChildItem -LiteralPath $Bundle -Recurse -File)) {
        if ($f.Name -ne 'bundle.json' -and -not $listed.ContainsKey($f.FullName)) { throw "unlisted bundle file: $($f.FullName)" }
    }
    if (Test-Path -LiteralPath $Job) { throw "job folder exists: $Job" }
    foreach ($d in @('projects', 'stimuli', 'out')) { New-Item -ItemType Directory -Force -Path (Join-Path $Job $d) | Out-Null }
    Copy-Item -Path (Join-Path $Bundle 'stimuli\*') -Destination (Join-Path $Job 'stimuli')
    $staged = @()
    foreach ($proj in $m.projects) {
        $text = [IO.File]::ReadAllText((Join-Path $Bundle ($proj.file -replace '/', '\')))
        Test-GoldenRpp -Text $text
        $text = $text.Replace('@@JOB@@', $Job).Replace('@@OUT@@', (Join-Path $Job 'out'))
        if ($text.Contains('@@')) { throw "unresolved token in $($proj.file)" }
        $dst = Join-Path $Job ("projects\" + $proj.id + '.rpp')
        [IO.File]::WriteAllText($dst, $text, $script:Utf8NoBom)
        New-Item -ItemType Directory -Force -Path (Join-Path $Job ("out\" + $proj.id)) | Out-Null
        $staged += $dst
    }
    return ,$staged
}

function New-GoldenResourceDir {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$MainResource,
          [Parameter(Mandatory)][int]$DummyMode, [int]$Rate = 96000, [string]$VstPath = '')
    if ($DummyMode -eq 3) { throw 'audio mode 3 is ASIO: refused' }
    if (Test-Path -LiteralPath $Path) { throw "resource folder exists: $Path" }
    New-Item -ItemType Directory -Path $Path | Out-Null
    # An explicit VST path keeps the render instance from scanning (loading) other
    # plug-ins, e.g. the predecessor's VST3s in the default VST3 folder.
    $vst = @(); if ($VstPath) { $vst = @("vstpath64=$VstPath") }
    $ini = @('[REAPER]', 'loadlastproj=0', 'autosave=0', 'verchk=0') + $vst + @('', '[audioconfig]', "mode=$DummyMode", "dummy_srate=$Rate", 'dummy_blocksize=64', 'allow_sr_override=1', '')
    [IO.File]::WriteAllText((Join-Path $Path 'reaper.ini'), ($ini -join "`r`n"), [Text.Encoding]::ASCII)
    foreach ($rel in @('Effects\utility\volume_pan', 'Effects\loser\MGA_JSLimiterST')) {
        $dst = Join-Path $Path $rel
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dst) | Out-Null
        Copy-Item -LiteralPath (Join-Path $MainResource $rel) -Destination $dst
    }
    foreach ($name in @('reaper-license.rk', 'reaper-reginfo2.ini')) {
        $src = Join-Path $MainResource $name
        if (Test-Path -LiteralPath $src) { Copy-Item -LiteralPath $src -Destination (Join-Path $Path $name) }
    }
    return (Join-Path $Path 'reaper.ini')
}

function Get-GoldenAsioHolders {
    param([Parameter(Mandatory)][string]$Module)
    $out = & tasklist.exe /m $Module /fo csv /nh
    return ,@($out | Where-Object { $_ -like '"*' } | ForEach-Object { $c = $_.Trim('"') -split '","'; "$($c[0]):$($c[1])" })
}

function Write-GoldenStatus {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$State, [AllowEmptyCollection()][object[]]$Results = @())
    $tmp = "$Path.tmp"
    $json = [pscustomobject]@{ state = $State; at = (Get-Date).ToUniversalTime().ToString('o'); results = @($Results) } | ConvertTo-Json -Depth 6
    [IO.File]::WriteAllText($tmp, $json, $script:Utf8NoBom)
    Move-Item -LiteralPath $tmp -Destination $Path -Force
}

function Write-GoldenRequest {
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$Kind, [hashtable]$Fields = @{})
    $id = '{0}-{1}' -f $Kind, (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssfff')
    $req = @{ id = $id; kind = $Kind }
    foreach ($k in $Fields.Keys) { $req[$k] = $Fields[$k] }
    [IO.File]::WriteAllText((Join-Path $Root 'queue\request.json'), ($req | ConvertTo-Json -Depth 5), $script:Utf8NoBom)
    return $id
}

function Invoke-GoldenRenderQueue {
    param([Parameter(Mandatory)][string]$Reaper, [Parameter(Mandatory)][string]$Ini, [Parameter(Mandatory)][string[]]$Projects,
          [Parameter(Mandatory)][string]$StatusPath, [Parameter(Mandatory)][string]$StopFile, [int]$TimeoutSec = 600)
    $results = @()
    foreach ($p in $Projects) {
        if (Test-Path -LiteralPath $StopFile) { $results += [pscustomobject]@{ project = $p; state = 'skipped-stop' }; continue }
        $sw = [Diagnostics.Stopwatch]::StartNew()
        $proc = Start-Process -FilePath $Reaper -PassThru -ArgumentList @('-newinst', '-nosplash', '-ignoreerrors', '-cfgfile', "`"$Ini`"", '-renderproject', "`"$p`"")
        $null = $proc.Handle   # keeps ExitCode readable after the exit (Windows PowerShell 5.1)
        $audio = @{}
        while (-not $proc.WaitForExit(100) -and $sw.Elapsed.TotalSeconds -lt $TimeoutSec) {
            # Evidence of the audio system the instance opened (dsound, WASAPI, KS, WaveOut or any ASIO module).
            try { $proc.Refresh(); foreach ($mod in $proc.Modules) { if ($mod.ModuleName -match '(?i)^(dsound\.dll|audioses\.dll|ksuser\.dll|mmdevapi\.dll|wdmaud\.drv)$|asio') { $audio[$mod.ModuleName.ToLowerInvariant()] = $true } } } catch { }
        }
        $done = $proc.HasExited
        $results += [pscustomobject]@{ project = $p; state = $(if ($done) { 'exited' } else { 'hung' }); exit = $(if ($done) { $proc.ExitCode } else { $null }); pid = $proc.Id; seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1); audio_modules = @($audio.Keys | Sort-Object) }
        Write-GoldenStatus -Path $StatusPath -State 'running' -Results $results
        if (-not $done) { break }   # never end it by force: the operator closes the dialog
    }
    $final = if (@($results | Where-Object { $_.state -eq 'hung' }).Count -gt 0) { 'hung' } else { 'done' }
    Write-GoldenStatus -Path $StatusPath -State $final -Results $results
}

function Watch-GoldenRender {
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$RequestId, [Parameter(Mandatory)][string]$AsioModule, [Parameter(Mandatory)][int]$TimeoutSec)
    $status = Join-Path $Root "status\$RequestId.json"; $stop = Join-Path $Root 'queue\stop'
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ($true) {
        $holders = Get-GoldenAsioHolders -Module $AsioModule
        if ($holders.Count -gt 0) { New-Item -ItemType File -Force -Path $stop | Out-Null; return [pscustomobject]@{ outcome = 'asio-alarm'; holders = $holders } }
        if (Test-Path -LiteralPath $status) {
            try { $s = Get-Content -LiteralPath $status -Raw | ConvertFrom-Json } catch { $s = $null }
            if ($s -and @('done', 'hung', 'failed') -contains $s.state) { return [pscustomobject]@{ outcome = $s.state; status = $s } }
        }
        if ((Get-Date) -gt $deadline) { New-Item -ItemType File -Force -Path $stop | Out-Null; return [pscustomobject]@{ outcome = 'timeout' } }
        Start-Sleep -Milliseconds 500
    }
}

function Request-GoldenCloseRender {
    param([Parameter(Mandatory)][string]$IniPath)
    $procs = @(Get-CimInstance Win32_Process -Filter "Name = 'reaper.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($IniPath) })
    foreach ($p in $procs) { $gp = Get-Process -Id $p.ProcessId -ErrorAction SilentlyContinue; if ($gp) { [void]$gp.CloseMainWindow() } }
    return $procs.Count
}

function Register-GoldenTask {
    param([Parameter(Mandatory)][string]$Root, [string]$Name = 'iemmixer-golden')
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument ('-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "' + (Join-Path $Root 'bin\golden-task.ps1') + '"')
    $principal = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\$env:USERNAME" -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    Register-ScheduledTask -TaskName $Name -Action $action -Principal $principal -Settings $settings -Force | Out-Null
}

function Test-GoldenHttp {
    param([Parameter(Mandatory)][string]$Uri)
    try { return [int](Invoke-WebRequest -UseBasicParsing -Uri $Uri -TimeoutSec 5 -MaximumRedirection 0).StatusCode }
    catch [System.Net.WebException] { if ($_.Exception.Response) { return [int]$_.Exception.Response.StatusCode } else { return 0 } }
}

function Wait-GoldenProcessGone {
    param([Parameter(Mandatory)][string]$Name, [int]$Seconds = 30)
    $deadline = (Get-Date).AddSeconds($Seconds)
    while (@(Get-Process -Name $Name -ErrorAction SilentlyContinue).Count -gt 0) {
        if ((Get-Date) -gt $deadline) { throw "$Name is still running after $Seconds s (nothing is ended by force)" }
        Start-Sleep -Milliseconds 500
    }
}

function Invoke-GoldenSaveQuit {
    param([Parameter(Mandatory)][string]$Http, [Parameter(Mandatory)][string]$Project, [Parameter(Mandatory)][string]$AsioModule)
    $before = (Get-Item -LiteralPath $Project).LastWriteTimeUtc
    Invoke-WebRequest -UseBasicParsing -Uri "$Http/_/40026" -TimeoutSec 10 | Out-Null
    $deadline = (Get-Date).AddSeconds(15)
    while ((Get-Item -LiteralPath $Project).LastWriteTimeUtc -eq $before) {
        if ((Get-Date) -gt $deadline) { throw 'REAPER did not save within 15 s; nothing else was done' }
        Start-Sleep -Milliseconds 250
    }
    try { Invoke-WebRequest -UseBasicParsing -Uri "$Http/_/40004" -TimeoutSec 5 | Out-Null } catch { }   # REAPER may drop the connection while quitting
    Wait-GoldenProcessGone -Name 'reaper' -Seconds 30
    $holders = Get-GoldenAsioHolders -Module $AsioModule
    if ($holders.Count -gt 0) { throw "the ASIO module is still held: $($holders -join ', ')" }
    [pscustomobject]@{ saved = $true; quit = $true }
}

function Invoke-GoldenBringBack {
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$StartTask, [Parameter(Mandatory)][string]$Http,
          [Parameter(Mandatory)][string]$AppExe, [Parameter(Mandatory)][string]$AppProcess, [Parameter(Mandatory)][string]$AppHttp,
          [bool]$WantReaper = $true, [bool]$WantApp = $true)
    $render = @(Get-CimInstance Win32_Process -Filter "Name = 'reaper.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($Root) })
    if ($render.Count -gt 0) { throw 'a render instance still runs: close it (MCP or close-render) before REAPER may start' }
    $reaperUp = $false; $appUp = $false
    if ($WantReaper) {
        if (@(Get-Process reaper -ErrorAction SilentlyContinue).Count -eq 0) { Start-ScheduledTask -TaskName $StartTask }
        $deadline = (Get-Date).AddSeconds(90)
        while (-not $reaperUp -and (Get-Date) -lt $deadline) { $reaperUp = ((Test-GoldenHttp -Uri "$Http/_/NTRACK") -eq 200); if (-not $reaperUp) { Start-Sleep -Seconds 1 } }
        if (-not $reaperUp) { throw 'REAPER did not answer within 90 s after its start task' }
    }
    if ($WantApp) {
        if (@(Get-Process -Name $AppProcess -ErrorAction SilentlyContinue).Count -eq 0) {
            [void](Write-GoldenRequest -Root $Root -Kind 'start-app' -Fields @{ exe = $AppExe })
            Start-ScheduledTask -TaskName 'iemmixer-golden'
        }
        $deadline = (Get-Date).AddSeconds(60)
        while (-not $appUp -and (Get-Date) -lt $deadline) { $code = Test-GoldenHttp -Uri $AppHttp; $appUp = ($code -gt 0 -and $code -lt 500); if (-not $appUp) { Start-Sleep -Seconds 1 } }
        if (-not $appUp) { throw 'the predecessor app did not answer within 60 s' }
    }
    [pscustomobject]@{ reaper = $reaperUp; app = $appUp }
}

function Get-GoldenMeterSamples {
    param([Parameter(Mandatory)][string]$Http, [int]$Seconds = 60)
    $samples = @()
    for ($i = 0; $i -lt $Seconds; $i++) {
        $samples += (Invoke-WebRequest -UseBasicParsing -Uri "$Http/_/NTRACK;TRACK" -TimeoutSec 5).Content
        Start-Sleep -Seconds 1
    }
    return ,$samples
}

function Get-GoldenFileHashes {
    param([Parameter(Mandatory)][string]$Path)
    $root = [IO.Path]::GetFullPath($Path).TrimEnd('\')
    return ,@(Get-ChildItem -LiteralPath $root -Recurse -File | Sort-Object FullName | ForEach-Object {
        [pscustomobject]@{ rel = $_.FullName.Substring($root.Length + 1).Replace('\', '/'); sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(); size = $_.Length }
    })
}

Export-ModuleMember -Function *-Golden*
