# Integration Tests for Checkpoint & UTXO Rollback

This directory contains integration tests for the checkpoint system and UTXO rollback functionality.

## Test Scripts

### PowerShell (Windows)
```powershell
.\test_checkpoint_rollback.ps1
```

### Bash (Linux/Mac)
```bash
chmod +x test_checkpoint_rollback.sh
./test_checkpoint_rollback.sh
```

## What These Tests Validate

### 1. Checkpoint System
- ✓ Genesis checkpoint exists
- ✓ Checkpoint validation infrastructure present
- ✓ Checkpoint-related logging active

### 2. Block Addition
- ✓ Blocks are being added to chain
- ✓ Height increases over time
- ✓ Block hashes are tracked correctly

### 3. Feature Presence
- ✓ Checkpoint system code is active
- ✓ Reorganization infrastructure present  
- ✓ Chain work tracking operational
- ✓ UTXO manager functional

## Test Output

Tests will show:
- 🟢 **Green** - Feature working correctly
- 🟡 **Yellow** - Warning or inconclusive (may be normal)
- 🔴 **Red** - Error or failure

## Requirements

- Rust/Cargo installed
- Project compiled (`cargo build --release`)
- Available ports: 8332 (RPC), 9333 (P2P)
- curl and jq (for bash script)
- PowerShell 5.1+ (for PowerShell script)

## Test Duration

- Build time: ~2-5 minutes
- Test execution: ~1-2 minutes
- Total: ~3-7 minutes

## Troubleshooting

### Node Won't Start
- Check if ports 8332/9333 are available
- Look at logs in `$TEMP\timecoin_integration_test\node1\node.log` (Windows)
- or `/tmp/timecoin_integration_test/node1/node.log` (Linux)

### Tests Timeout
- Increase wait times in script
- Check system resources (CPU/memory)
- Verify no firewall blocking localhost connections

### No Block Production
- Normal for isolated node (no peers)
- Tests will show "INCONCLUSIVE" but won't fail
- For full testing, connect to testnet peers

## Manual Testing

For more comprehensive testing, see `MANUAL_TESTING_GUIDE.md`

## Adding New Tests

To add more tests:

1. Create new function: `Test-YourFeature` (PowerShell) or `test_your_feature` (Bash)
2. Add test call in `Main` function
3. Follow existing test pattern
4. Use Write-Success/Write-Warning/Write-Error for output

## Test Limitations

These integration tests:
- ✓ Verify infrastructure is present
- ✓ Check basic functionality
- ✓ Validate logging and tracking

These tests do NOT:
- ✗ Trigger actual reorganizations (requires multiple nodes)
- ✗ Test deep rollbacks (requires long chains)
- ✗ Validate checkpoint boundaries (requires checkpoint blocks)
- ✗ Test mempool replay (requires transaction pool activity)

For comprehensive testing, use manual testnet validation or multi-node scenarios.

## CI/CD Integration

To run in CI:

```yaml
# .github/workflows/integration-tests.yml
- name: Run Integration Tests
  run: |
    cargo build --release
    pwsh tests/integration/test_checkpoint_rollback.ps1
```

## Next Steps

After running these tests:
1. Review test output for any warnings
2. Check node logs for unexpected errors
3. If all tests pass, proceed to manual testnet validation
4. Consider adding more specific tests for edge cases
