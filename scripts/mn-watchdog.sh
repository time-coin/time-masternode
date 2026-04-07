#!/bin/bash
# mn-watchdog.sh — Masternode registration watchdog for TIME Coin
#
# Monitors this node's masternode registration via RPC. When the node
# is de-registered (evicted by an attacker, network partition, or crash),
# automatically runs `systemctl restart timed` so the daemon re-registers.
#
# "Not a masternode" status is treated as de-registration only when
# masternode=1 is present in time.conf; if not configured it is skipped.
#
# Design:
#   - Polls `masternodestatus` every POLL_INTERVAL seconds
#   - Requires FAIL_THRESHOLD consecutive "not active" readings before
#     restarting (avoids false positives from brief RPC hiccups)
#   - Enforces RESTART_COOLDOWN between restarts (avoids restart loops)
#   - Startup grace: waits STARTUP_GRACE seconds after timed last started
#     before monitoring (avoids restarting a daemon that is still initializing)
#   - Logs all events to systemd journal via `logger`
#
# Installation (run as root):
#   sudo cp scripts/mn-watchdog.sh /usr/local/bin/mn-watchdog
#   sudo chmod +x /usr/local/bin/mn-watchdog
#   sudo cp scripts/mn-watchdog.service /etc/systemd/system/
#   sudo systemctl daemon-reload
#   sudo systemctl enable --now mn-watchdog
#
# Manual test run:
#   sudo bash scripts/mn-watchdog.sh --dry-run
#
# Options:
#   --testnet              Use testnet RPC port (24101)
#   --poll SECS            Poll interval in seconds (default: 30)
#   --fail-threshold N     Consecutive "not active" readings before restart (default: 1)
#   --restart-cooldown N   Min seconds between restarts (default: 600)
#   --startup-grace N      Seconds to wait after watchdog launches before monitoring (default: 3)
#   --dry-run              Log what would happen but do not restart
#   -h, --help             Show this help

set -uo pipefail

# ── Defaults ──────────────────────────────────────────────────────────────────
POLL_INTERVAL=30
FAIL_THRESHOLD=1
RESTART_COOLDOWN=600
STARTUP_GRACE=3
DRY_RUN=0
NETWORK="mainnet"

# ── Argument parsing ───────────────────────────────────────────────────────────
usage() {
    grep '^#' "$0" | grep -v '!/bin/bash' | sed 's/^# \{0,1\}//' | \
        sed -n '/^mn-watchdog/,/^  -h/p'
    exit 0
}

while [[ $# -gt 0 ]]; do
    case $1 in
        --testnet)            NETWORK="testnet";  shift ;;
        --poll)               POLL_INTERVAL="$2"; shift 2 ;;
        --fail-threshold)     FAIL_THRESHOLD="$2"; shift 2 ;;
        --restart-cooldown)   RESTART_COOLDOWN="$2"; shift 2 ;;
        --startup-grace)      STARTUP_GRACE="$2"; shift 2 ;;
        --dry-run)            DRY_RUN=1; shift ;;
        -h|--help)            usage ;;
        *) echo "Unknown option: $1"; usage ;;
    esac
done

# ── Logger tag ─────────────────────────────────────────────────────────────────
TAG="mn-watchdog"
log()  { logger -t "$TAG" -- "$*"; echo "$(date '+%Y-%m-%d %H:%M:%S') $*"; }
logw() { logger -t "$TAG" -p user.warning  -- "WARN  $*"; echo "$(date '+%Y-%m-%d %H:%M:%S') WARN  $*"; }
loge() { logger -t "$TAG" -p user.err      -- "ERROR $*"; echo "$(date '+%Y-%m-%d %H:%M:%S') ERROR $*" >&2; }

# ── Locate time-cli ────────────────────────────────────────────────────────────
CLI=""
for candidate in \
    "$(command -v time-cli 2>/dev/null)" \
    /usr/local/bin/time-cli \
    /usr/bin/time-cli \
    /opt/timecoin/bin/time-cli \
    "$(dirname "$0")/../target/release/time-cli" \
    ./target/release/time-cli
do
    if [ -x "$candidate" ]; then
        CLI="$candidate"
        break
    fi
done

if [ -z "$CLI" ]; then
    loge "time-cli not found in PATH or common locations — cannot monitor"
    exit 1
fi

CLI_CMD="$CLI"
[ "$NETWORK" = "testnet" ] && CLI_CMD="$CLI --testnet"

log "Starting ($NETWORK) | poll=${POLL_INTERVAL}s fail-threshold=${FAIL_THRESHOLD} cooldown=${RESTART_COOLDOWN}s grace=${STARTUP_GRACE}s dry-run=${DRY_RUN}"
log "Using CLI: $CLI_CMD"

# ── State ──────────────────────────────────────────────────────────────────────
fail_streak=0           # consecutive non-active readings
last_restart_ts=0       # unix timestamp of last restart we triggered
total_restarts=0
watchdog_start_ts=$(date +%s)   # used for one-time startup grace

# ── Check if timed has been running long enough (startup grace) ────────────────
service_started_ago() {
    # Returns the number of seconds since timed was last (re)started.
    # Returns a large number if the service is not running.
    local since
    since=$(systemctl show timed --property=ActiveEnterTimestamp --value 2>/dev/null)
    if [ -z "$since" ] || [ "$since" = "n/a" ]; then
        echo "9999"
        return
    fi
    local started_ts
    started_ts=$(date -d "$since" +%s 2>/dev/null) || { echo "9999"; return; }
    echo $(( $(date +%s) - started_ts ))
}

# ── Main loop ──────────────────────────────────────────────────────────────────
while true; do
    sleep "$POLL_INTERVAL"

    # 1. Daemon must be running.
    if ! systemctl is-active --quiet timed 2>/dev/null; then
        logw "timed is not active — skipping (systemd will handle restart if configured)"
        fail_streak=0
        continue
    fi

    # 2. One-time startup grace so the watchdog doesn't fire immediately after
    #    being launched while timed is still initializing.
    #    After this window passes once, detection is immediate; subsequent
    #    restart cooldown is handled by RESTART_COOLDOWN below.
    watchdog_age=$(( $(date +%s) - watchdog_start_ts ))
    if [ "$watchdog_age" -lt "$STARTUP_GRACE" ]; then
        remaining=$(( STARTUP_GRACE - watchdog_age ))
        log "Startup grace (watchdog age ${watchdog_age}s, grace=${STARTUP_GRACE}s, ${remaining}s remaining)"
        fail_streak=0
        continue
    fi

    # 3. Query masternodestatus.
    status_json=$($CLI_CMD masternodestatus 2>/dev/null) || status_json=""
    if [ -z "$status_json" ]; then
        logw "RPC call failed (streak: $((fail_streak + 1))/$FAIL_THRESHOLD)"
        fail_streak=$(( fail_streak + 1 ))
    else
        # Parse the "status" field.  Uses jq if available, falls back to grep.
        if command -v jq &>/dev/null; then
            mn_status=$(echo "$status_json" | jq -r '.status // "unknown"' 2>/dev/null)
            is_active=$(echo "$status_json"  | jq -r '.is_active // false'  2>/dev/null)
        else
            mn_status=$(echo "$status_json" | grep -oP '"status"\s*:\s*"\K[^"]+' | head -1)
            is_active=$(echo "$status_json" | grep -oP '"is_active"\s*:\s*\K(true|false)' | head -1)
        fi

        if [ "$mn_status" = "active" ] && [ "$is_active" = "true" ]; then
            # Healthy — reset streak.
            [ "$fail_streak" -gt 0 ] && log "Masternode active again — clearing failure streak"
            fail_streak=0
        elif [ "$mn_status" = "Not a masternode" ]; then
            # The daemon says it is not registered.  This can mean either:
            #   (a) masternode=0 in time.conf  → operator intentionally disabled; don't restart
            #   (b) masternode=1 but the node was de-registered by an attack → must restart
            # Distinguish by checking the config file directly.
            conf_file=""
            if [ "$NETWORK" = "testnet" ]; then
                conf_file="${HOME}/.timecoin/testnet/time.conf"
            else
                conf_file="${HOME}/.timecoin/time.conf"
            fi
            # Accept both "masternode=1" and "masternode = 1" (with spaces)
            if grep -qsE '^\s*masternode\s*=\s*1' "$conf_file" 2>/dev/null; then
                logw "Node de-registered (config has masternode=1 but status='Not a masternode') — treating as de-registration, streak: $((fail_streak + 1))/$FAIL_THRESHOLD"
                fail_streak=$(( fail_streak + 1 ))
            else
                logw "Node reports 'Not a masternode' and masternode is not enabled in config — not a de-registration event; check time.conf"
                fail_streak=0
            fi
        else
            # Registered but is_active=false, or unrecognized status.
            logw "Masternode NOT active (status=${mn_status:-unknown} is_active=${is_active:-unknown}) — streak: $((fail_streak + 1))/$FAIL_THRESHOLD"
            fail_streak=$(( fail_streak + 1 ))
        fi
    fi

    # 4. Threshold reached — restart timed.
    if [ "$fail_streak" -ge "$FAIL_THRESHOLD" ]; then
        now=$(date +%s)
        since_last=$(( now - last_restart_ts ))

        if [ "$since_last" -lt "$RESTART_COOLDOWN" ]; then
            remaining=$(( RESTART_COOLDOWN - since_last ))
            logw "Threshold reached but restart cooldown active (${remaining}s remaining) — waiting"
            # Don't reset fail_streak; keep accumulating so we retry the moment cooldown expires.
            continue
        fi

        total_restarts=$(( total_restarts + 1 ))

        if [ "$DRY_RUN" -eq 1 ]; then
            log "DRY-RUN: would run 'systemctl restart timed' (restart #${total_restarts})"
        else
            log "🔁 De-registration detected after ${fail_streak} consecutive checks — restarting timed (restart #${total_restarts})"
            if systemctl restart timed; then
                log "✅ systemctl restart timed succeeded"
            else
                loge "systemctl restart timed FAILED (exit $?)"
            fi
        fi

        last_restart_ts=$now
        fail_streak=0
    fi
done
