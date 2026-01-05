## Fork Resolution Issue - Manual Recovery

**Problem:** Node stuck on wrong fork and cannot sync with network due to common ancestor detection failure.

**Symptoms:**
- Node at different height than peers (e.g., 5154 vs 5157)
- Repeated fork detection warnings with previous_hash mismatch
- Chain reorganization failing with 'previous_hash mismatch' errors

**Immediate Solution (For Stuck Nodes):**
Since the common ancestor detection bug has been fixed in commit ea0ab2b, nodes that are currently stuck on the wrong fork need manual intervention:

1. **Stop the node**
2. **Delete blockchain data** (keeps wallet):
   - Linux: \m -rf ~/.timecoin/testnet/db\
   - Windows: Delete \%APPDATA%\timecoin\testnet\db\
3. **Update to latest version** with the fix
4. **Restart node** - it will resync from genesis with correct fork resolution

**Prevention:**
The fix ensures that future forks will be properly detected and resolved automatically. Nodes will now correctly identify the true common ancestor and accept the longer chain.

**Root Cause:**
The \ind_common_ancestor()\ function incorrectly assumed that if all competing blocks had heights greater than H, then block H must be the common ancestor. This failed when the node's block H differed from the network's block H (indicating the fork started earlier).

