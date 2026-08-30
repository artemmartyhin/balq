# End-to-end demo on the Plataberget devnet, in one run:
#   build -> fresh archive -> watch -> sync --follow (background)
#   -> send real transactions to the test contract -> see them land
#   -> diff by variable name -> backfill to the deploy -> verify vs journal
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

Step 1 "fresh archive, watch $C from the node's head"
Remove-Item $DATA, demo-sync.log, demo-sync.err -ErrorAction SilentlyContinue
$w = (& $B --data $DATA --json watch $C --rpc $RPC | ConvertFrom-Json)
$start = [int64]$w.from
Write-Host "    watching from block $start"

Step 2 "sync --follow in the background, then send $Pokes transaction(s) to the contract"
$follow = Start-Process -FilePath $B -ArgumentList "--data $DATA sync --rpc $RPC --follow --poll 3" `
    -RedirectStandardOutput demo-sync.log -RedirectStandardError demo-sync.err -PassThru -NoNewWindow
try {
    node poke.mjs poke $Pokes 4
    if ($LASTEXITCODE -ne 0) { throw "poke failed (wallet funded? gateway up?)" }
    Write-Host "    waiting for the follower to pick the last block up..."
    Start-Sleep -Seconds 12
} finally {
    Stop-Process -Id $follow.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
}
Write-Host "    what the follower saw (demo-sync.log):" -ForegroundColor DarkGray
Get-Content demo-sync.log | ForEach-Object { "      $_" }

Step 3 "status, then the diff of every variable between the watch start and the head"
Run status
$head = [int64]((& $B --data $DATA --json status | ConvertFrom-Json).head.number)
Run diff $C --from $start --to $head --layout $LAYOUT
Run history $C --slot 0 --range "$start..$($head + 1)"

Step 4 "backfill: walk older blocks back to the deploy (no proofs, no archive node)"
Run backfill $C --rpc $RPC --chunk 500
Run status

Step 5 "the same variable at the deploy block, at the watch start, and now"
$field = "balances[$($env_['ADDR'])]"
foreach ($blk in @(114562, $start, $head)) { Run get $C --layout $LAYOUT --field $field --block $blk }

Step 6 "verify every value the archive holds against the journal written by the sender"
Run verify --journal journal.jsonl

Write-Host ""
Write-Host "done. archive: $DATA ($([math]::Round((Get-Item $DATA).Length / 1MB, 1)) MB) - delete it or keep reading from it." -ForegroundColor Green
