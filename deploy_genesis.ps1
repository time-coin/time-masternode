# Deploy genesis.testnet.json to all servers (PowerShell version)

$servers = @(
    "root@50.28.104.50",
    "root@64.91.241.10",
    "root@69.167.168.176",
    "root@165.232.154.150",
    "root@165.84.215.117",
    "root@178.128.199.144"
)

$genesisFile = "genesis.testnet.json"

if (-not (Test-Path $genesisFile)) {
    Write-Host "❌ Error: $genesisFile not found in current directory" -ForegroundColor Red
    exit 1
}

Write-Host "📤 Deploying genesis.testnet.json to all servers..." -ForegroundColor Cyan

foreach ($server in $servers) {
    Write-Host ""
    Write-Host "📡 Deploying to $server..." -ForegroundColor Yellow
    
    # Copy to /root/ directory (most reliable location)
    $scpResult = scp $genesisFile "${server}:/root/"
    if ($LASTEXITCODE -eq 0) {
        Write-Host "✅ Successfully deployed to ${server}:/root/" -ForegroundColor Green
        
        # Also try to copy to /etc/timecoin/ if it exists
        ssh $server "mkdir -p /etc/timecoin && cp /root/$genesisFile /etc/timecoin/ 2>/dev/null || true"
    } else {
        Write-Host "❌ Failed to deploy to $server" -ForegroundColor Red
    }
}

Write-Host ""
Write-Host "✅ Deployment complete!" -ForegroundColor Green
Write-Host ""
Write-Host "Next steps:" -ForegroundColor Cyan
Write-Host "1. Restart all nodes: foreach (`$s in `$servers) { ssh `$s 'sudo systemctl restart timed' }"
Write-Host "2. Check logs: ssh root@50.28.104.50 'journalctl -u timed -f'"
