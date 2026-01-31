# Check if block 1 is being produced
journalctl -u timed -n 200 --no-pager | grep -E "Block 1|height 1|Selected as block producer"

# Check masternode registration and leader selection
journalctl -u timed -n 200 --no-pager | grep -E "Bootstrap mode|leader selection|registered masternodes"

# Check voting activity
journalctl -u timed -n 200 --no-pager | grep -E "prepare vote|precommit vote|consensus reached"

# Check for any production errors
journalctl -u timed -n 200 --no-pager | grep -E "Cannot produce|Skipping|waiting for"

# Get current height and recent activity (last 100 lines)
journalctl -u timed -n 100 --no-pager

# Check what height nodes are at
journalctl -u timed -n 50 --no-pager | grep -E "height|Height"
