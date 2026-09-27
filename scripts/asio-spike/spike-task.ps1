#Requires -Version 5.1
# Entry point of the Interactive task \iemmixer\iemmixer-asio-spike: runs one
# request from queue\request.json in the console session (S1a design note §5).
# Every failure leaves a status: status\<id>.json, or status\request-unreadable.json
# when the request carries no readable id.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'SpikePc.psm1') -Force
$root = Split-Path -Parent $PSScriptRoot
$status = Join-Path $root 'status\request-unreadable.json'
try {
    $text = Get-Content -LiteralPath (Join-Path $root 'queue\request.json') -Raw
    if ($text -match '"id"\s*:\s*"([a-z]+-[0-9]{8}T[0-9]{9})"') { $status = Join-Path $root ("status\" + $Matches[1] + '.json') }
    $req = $text | ConvertFrom-Json
    $id = $req.PSObject.Properties['id']
    if (-not $id -or "$($id.Value)" -notmatch '^[a-z]+-[0-9]{8}T[0-9]{9}$') { throw 'the request has no valid id' }
    $kind = $req.PSObject.Properties['kind']
    if (-not $kind -or $kind.Value -ne 'spike') { throw "unknown request kind: $(if ($kind) { $kind.Value })" }
    Invoke-SpikeRun -Root $root -Request $req
} catch {
    Write-GoldenStatus -Path $status -State 'failed' -Results @([pscustomobject]@{ error = "$_" })
    exit 1
}
