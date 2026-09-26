#Requires -Version 5.1
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'GoldenPc.psm1') -Force
$root = Split-Path -Parent $PSScriptRoot
$req = Get-Content -LiteralPath (Join-Path $root 'queue\request.json') -Raw | ConvertFrom-Json
$status = Join-Path $root ("status\" + $req.id + '.json')
try {
    switch ($req.kind) {
        'render' {
            Invoke-GoldenRenderQueue -Reaper $req.reaper -Ini $req.ini -Projects @($req.projects) -StatusPath $status -StopFile (Join-Path $root 'queue\stop') -TimeoutSec ([int]$req.timeout)
        }
        'audiocfg' {
            Start-Process -FilePath $req.reaper -ArgumentList @('-newinst', '-nosplash', '-audiocfg', '-cfgfile', "`"$($req.ini)`"") | Out-Null
            Write-GoldenStatus -Path $status -State 'started'
        }
        'close-render' {
            $n = Request-GoldenCloseRender -IniPath $req.ini
            Write-GoldenStatus -Path $status -State 'done' -Results @([pscustomobject]@{ closed = $n })
        }
        'start-app' {
            Start-Process -FilePath $req.exe -WorkingDirectory (Split-Path -Parent $req.exe) | Out-Null
            Write-GoldenStatus -Path $status -State 'started'
        }
        default { throw "unknown request kind: $($req.kind)" }
    }
} catch {
    Write-GoldenStatus -Path $status -State 'failed' -Results @([pscustomobject]@{ error = "$_" })
    exit 1
}
