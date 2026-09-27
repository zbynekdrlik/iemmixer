#Requires -Version 5.1
# Entry point of the Interactive task \iemmixer\iemmixer-asio-spike: runs one
# request from queue\request.json in the console session (S1a design note §5).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'SpikePc.psm1') -Force
$root = Split-Path -Parent $PSScriptRoot
$req = Get-Content -LiteralPath (Join-Path $root 'queue\request.json') -Raw | ConvertFrom-Json
$status = Join-Path $root ("status\" + $req.id + '.json')
try {
    if ($req.kind -ne 'spike') { throw "unknown request kind: $($req.kind)" }
    Invoke-SpikeRun -Root $root -Request $req
} catch {
    Write-GoldenStatus -Path $status -State 'failed' -Results @([pscustomobject]@{ error = "$_" })
    exit 1
}
