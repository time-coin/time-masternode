# Git Commands for Review and Deployment

## Review Changes

```bash
# View all modified files
git status

# Review specific changes
git diff src/main.rs
git diff src/blockchain.rs

# View changes in a prettier format
git diff --color-words src/main.rs
git diff --color-words src/blockchain.rs
```

## Commit Changes

```bash
# Stage the modified files
git add src/main.rs
git add src/blockchain.rs

# Stage documentation
git add IMPLEMENTATION_SUMMARY.md
git add IMPROVEMENTS_APPLIED.md
git add CODE_CHANGES.md
git add GIT_COMMANDS.md

# Create commit with detailed message
git commit -m "Apply critical fixes and performance optimizations

Critical Fixes:
- Fix race condition in catchup block production
  Prevents duplicate blocks by re-checking height after lock acquisition
  
- Add transaction pool rejected entries cleanup
  Prevents memory leak from unbounded rejected tx cache

Performance Optimizations:
- Reduce disk flush frequency from every block to every 10th block
  Achieves 90% reduction in disk I/O operations
  
Changes:
- src/main.rs: Race condition fix, memory cleanup enhancement
- src/blockchain.rs: Disk flush optimization

All changes are backward-compatible, no breaking changes.
Code compiles successfully with cargo check."
```

## Create Feature Branch (Recommended)

```bash
# Create and switch to feature branch
git checkout -b feature/critical-fixes-2026-01

# Push to remote
git push -u origin feature/critical-fixes-2026-01

# Create pull request (via GitHub web UI or CLI)
gh pr create --title "Critical Fixes and Performance Optimizations" \
  --body "See IMPLEMENTATION_SUMMARY.md for details"
```

## Or Commit Directly to Main (If Approved)

```bash
# Make sure you're on main branch
git checkout main

# Stage and commit as shown above
git add src/main.rs src/blockchain.rs
git add *.md
git commit -m "..."

# Push to main
git push origin main
```

## Review Checklist

Before committing, verify:

- [ ] Code compiles: `cargo check`
- [ ] Tests pass: `cargo test` (if applicable)
- [ ] Clippy warnings: `cargo clippy`
- [ ] Review diff: `git diff`
- [ ] Documentation complete: All 3 .md files added
- [ ] Commit message is descriptive

## Deployment to Testnet

```bash
# On testnet server:
git pull origin feature/critical-fixes-2026-01

# Rebuild
cargo build --release

# Restart node
systemctl restart timed

# Monitor logs
tail -f /var/log/timed/timed.log
```

## Monitor After Deployment

```bash
# Check memory usage
free -h
watch -n 10 'ps aux | grep timed'

# Check disk I/O
iostat -x 1

# Monitor logs for race condition prevention
grep "already reached after lock" /var/log/timed/timed.log

# Check consensus stats
curl http://localhost:9332/stats | jq '.'
```

## Rollback (If Needed)

```bash
# Rollback to previous commit
git revert HEAD
git push origin main

# Or checkout previous commit
git checkout <previous-commit-hash>

# Rebuild and restart
cargo build --release
systemctl restart timed
```

## Tag Release (After Successful Deployment)

```bash
# Create annotated tag
git tag -a v1.x.x -m "Critical fixes and performance optimizations

- Fix race condition in catchup block production
- Add transaction pool cleanup automation  
- Optimize disk flush frequency (90% reduction)

See IMPLEMENTATION_SUMMARY.md for details."

# Push tag
git push origin v1.x.x
```

---

## Quick Commands Summary

```bash
# Complete workflow:
git status
git diff
git add src/main.rs src/blockchain.rs *.md
git commit -m "Apply critical fixes and performance optimizations"
git push origin main

# Or with feature branch:
git checkout -b feature/critical-fixes-2026-01
git add src/main.rs src/blockchain.rs *.md  
git commit -m "Apply critical fixes and performance optimizations"
git push -u origin feature/critical-fixes-2026-01
gh pr create
```
