# End-to-end demo on the Plataberget devnet, in one run:
#   build -> fresh archive -> `balq index` to the deploy -> follow in the
#   background while real transactions hit the contract -> diff by variable
#   name -> verify every value against the sender's journal.
#
#   .\demo.ps1              # ~5 min; needs testbed\.env (RPC, PK with funds) and deploy.json
#   .\demo.ps1 -SkipBuild   # reuse ..\target\release\balq.exe
#   .\demo.ps1 -Pokes 5     # more transactions
param([switch]$SkipBuild, [int]$Pokes = 3)

$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot
$B = Join-Path $PSScriptRoot "..\target\release\balq.exe"
$env_ = @{}
Get-Content .env | Where-Object { $_ -match "=" } | ForEach-Object { $k, $v = $_ -split "=", 2; $env_[$k.Trim()] = $v.Trim() }
$RPC = $env_["RPC"]
$C = (Get-Content deploy.json -Raw | ConvertFrom-Json).proxy
$DATA = "demo.redb"
$LAYOUT = "Playground.layout.json"

function Step($n, $t) { Write-Host ""; Write-Host "[$n] $t" -ForegroundColor Cyan }
function Run { Write-Host ("    > balq " + ($args -join " ")) -ForegroundColor DarkGray; & $B --data $DATA @args }

if (-not $SkipBuild) {
    Step 0 "build balq (release)"
    cargo build --release -p balq --manifest-path ..\Cargo.toml
    if ($LASTEXITCODE -ne 0) { throw "build failed" }
}

Step 1 "fresh archive: index $C to the deploy (--once)"
Remove-Item $DATA, demo-index.log, demo-index.err -ErrorAction SilentlyContinue
Run index $C --rpc $RPC --layout $LAYOUT --once
$start = [int64]((& $B --data $DATA --json status | ConvertFrom-Json).head.number)

Step 2 "index in the background (follow), then send $Pokes transaction(s)"
$follow = Start-Process -FilePath $B -ArgumentList "--data $DATA index $C --rpc $RPC --layout $LAYOUT --poll 3" `
    -RedirectStandardOutput demo-index.log -RedirectStandardError demo-index.err -PassThru -NoNewWindow
try {
    node poke.mjs poke $Pokes 4
    if ($LASTEXITCODE -ne 0) { throw "poke failed (wallet funded? gateway up?)" }
    Write-Host "    waiting for the follower to pick the last block up..."
    Start-Sleep -Seconds 12
} finally {
    Stop-Process -Id $follow.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
}
Write-Host "    what the follower printed (demo-index.log):" -ForegroundColor DarkGray
Get-Content demo-index.log | Select-Object -Skip 12 | ForEach-Object { "      $_" }

Step 3 "diff of every variable between the start of the run and the head, then verify"
$head = [int64]((& $B --data $DATA --json status | ConvertFrom-Json).head.number)
Run diff $C --from $start --to $head --layout $LAYOUT
Run verify --journal journal.jsonl

Write-Host ""
Write-Host "done. archive: $DATA ($([math]::Round((Get-Item $DATA).Length / 1MB, 1)) MB) - keep reading from it, or delete it." -ForegroundColor Green
