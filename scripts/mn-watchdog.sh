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
# IMPORTANT — RPC timeout vs. de-registration:
#   An RPC timeout does NOT necessarily mean the node is de-registered.
#   A node busy with fork resolution or a reconnection storm may exhaust
#   its tokio worker threads temporarily, making RPC unresponsive for up
#   to ~90 seconds while still running and registered. Restarting such a
#   node interrupts healthy operation and causes a ~60s reward eligibility
#   gap on every cycle (the "false-restart loop").
#
#   The watchdog distinguishes these cases using journalctl:
#   - If `timed` has logged within DAEMON_ACTIVE_SECS (default: 90s),
#     the daemon is alive and busy — RPC failures increment rpc_busy_streak.
#   - Only when rpc_busy_streak >= RPC_BUSY_MAX (default: 10 consecutive
#     polls, ~90s of unresponsiveness with recent log activity) OR when the
#     daemon has NOT logged recently does a restart trigger.
#   - When the RPC returns an explicit "not active" status (daemon is alive
#     and answering but the masternode is not registered), the standard
#     fail_streak / FAIL_THRESHOLD logic applies as before.
#
# Design:
#   - Polls `masternodestatus` every POLL_INTERVAL seconds
#   - Requires FAIL_THRESHOLD consecutive confirmed "not active" readings
#     before restarting (avoids false positives from brief RPC hiccups)
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
#   --testnet                Use testnet RPC port (24101)
#   --poll SECS              Poll interval in seconds (default: 3)
#   --fail-threshold N       Consecutive confirmed "not active" readings before restart (default: 3)
#   --restart-cooldown N     Min seconds between restarts (default: 60)
#   --startup-grace N        Seconds to wait after watchdog launches before monitoring (default: 3)
#   --post-restart-grace N   Seconds to skip polling after each restart while daemon initializes (default: 20)
#   --rpc-timeout N          Seconds to wait for time-cli RPC response (default: 5)
#   --rpc-busy-max N         Consecutive RPC timeouts while daemon is logging before restart (default: 5)
#   --daemon-active-secs N   Consider daemon alive if it logged within this many seconds (default: 60)
#   --dry-run                Log what would happen but do not restart
#   -h, --help               Show this help

set -uo pipefail

# ── Defaults ──────────────────────────────────────────────────────────────────
POLL_INTERVAL=3
FAIL_THRESHOLD=3       # confirmed "not active" RPC responses before restart
RESTART_COOLDOWN=60
STARTUP_GRACE=3
POST_RESTART_GRACE=20  # seconds to skip polling after each restart (daemon init time)
RPC_TIMEOUT=5          # seconds to wait for time-cli before treating as failure
RPC_BUSY_MAX=5         # consecutive timeouts while daemon is still logging → restart
DAEMON_ACTIVE_SECS=60  # consider daemon alive if it logged within this many seconds
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
        --post-restart-grace) POST_RESTART_GRACE="$2"; shift 2 ;;
        --rpc-timeout)        RPC_TIMEOUT="$2"; shift 2 ;;
        --rpc-busy-max)       RPC_BUSY_MAX="$2"; shift 2 ;;
        --daemon-active-secs) DAEMON_ACTIVE_SECS="$2"; shift 2 ;;
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
# Journal-only logger — no echo to screen (used for verbose diagnostics that
# would scroll important watchdog status messages off the terminal)
logj() { logger -t "$TAG" -- "$*"; }

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

log "Starting ($NETWORK) | poll=${POLL_INTERVAL}s fail-threshold=${FAIL_THRESHOLD} cooldown=${RESTART_COOLDOWN}s grace=${STARTUP_GRACE}s post-restart-grace=${POST_RESTART_GRACE}s rpc-timeout=${RPC_TIMEOUT}s busy-max=${RPC_BUSY_MAX} daemon-active-secs=${DAEMON_ACTIVE_SECS} dry-run=${DRY_RUN}"
log "Using CLI: $CLI_CMD"

# ── State ──────────────────────────────────────────────────────────────────────
fail_streak=0           # consecutive confirmed "not active" RPC responses
rpc_busy_streak=0       # consecutive RPC timeouts while daemon is still logging
last_restart_ts=0       # unix timestamp of last restart we triggered
total_restarts=0
cooldown_logged=0       # suppress repeated "cooldown active" log spam
watchdog_start_ts=$(date +%s)   # used for one-time startup grace

# ── Check if timed has logged recently (daemon alive but possibly busy) ────────
# Returns 0 (true) if timed has written a log entry within the last N seconds.
daemon_recently_active() {
    local max_age_secs=${1:-$DAEMON_ACTIVE_SECS}
    local last_log_line last_ts now_ts age
    # --output=short-unix gives a Unix timestamp as the first field.
    last_log_line=$(journalctl -u timed -n 1 --no-pager --output=short-unix 2>/dev/null)
    last_ts=$(echo "$last_log_line" | awk '{print $1}' | head -1)
    # short-unix may produce fractional timestamps (e.g. 1234567890.123456); strip decimals
    last_ts="${last_ts%%.*}"
    if [[ -z "$last_ts" || ! "$last_ts" =~ ^[0-9]+$ ]]; then
        return 1  # no parseable log entry found
    fi
    now_ts=$(date +%s)
    age=$(( now_ts - last_ts ))
    [ "$age" -le "$max_age_secs" ]
}

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

# ── Stall diagnostics ─────────────────────────────────────────────────────────
# Called on the first poll of every RPC stall (rpc_busy_streak == 1) and every
# 5 polls thereafter.  Dumps process stats and recent log lines so post-mortem
# analysis doesn't require a separate shell session on the node.
log_stall_diagnostics() {
    local streak=$1

    logj "── STALL DIAGNOSTICS (busy_streak=${streak}) ──"

    # Daemon PID and CPU/memory
    local pid
    pid=$(systemctl show timed --property=MainPID --value 2>/dev/null)
    if [[ -n "$pid" && "$pid" != "0" ]]; then
        local cpu_mem thread_count fd_count
        cpu_mem=$(ps -p "$pid" -o pid=,pcpu=,pmem=,vsz=,rss= 2>/dev/null || echo "unavailable")
        thread_count=$(ls /proc/"$pid"/task 2>/dev/null | wc -l || echo "?")
        fd_count=$(ls /proc/"$pid"/fd 2>/dev/null | wc -l || echo "?")
        logj "  PID=${pid}  cpu/mem: ${cpu_mem}  threads=${thread_count}  fds=${fd_count}"
    else
        logj "  PID: unavailable"
    fi

    # Open TCP connections to/from the daemon's P2P port
    local conn_count inbound outbound
    conn_count=$(ss -tnp 2>/dev/null | grep -c "timed" || echo "?")
    inbound=$(ss  -tnp 2>/dev/null | grep "timed" | grep -c ":24000 " || echo "?")
    outbound=$(ss -tnp 2>/dev/null | grep "timed" | grep -cv ":24000 " || echo "?")
    logj "  TCP connections: total=${conn_count} inbound~=${inbound} outbound~=${outbound}"

    # Last 25 log lines — strip timestamps down to HH:MM:SS for compactness
    logj "  -- last 25 log lines --"
    journalctl -u timed -n 25 --no-pager --output=short 2>/dev/null \
        | sed 's/^[A-Za-z]* [A-Za-z]* [0-9]* //' \
        | while IFS= read -r line; do logj "  $line"; done

    logj "── END STALL DIAGNOSTICS ──"
}

# ── Main loop ──────────────────────────────────────────────────────────────────
while true; do
    sleep "$POLL_INTERVAL"

    # 0. If already at threshold and in cooldown, skip polling entirely — don't increment.
    if [ "$fail_streak" -ge "$FAIL_THRESHOLD" ]; then
        now=$(date +%s)
        since_last=$(( now - last_restart_ts ))
        if [ "$since_last" -lt "$RESTART_COOLDOWN" ]; then
            remaining=$(( RESTART_COOLDOWN - since_last ))
            # Log once when cooldown starts, then again at 50% and final 5s — not every poll.
            if [ "$cooldown_logged" -eq 0 ]; then
                logw "Threshold reached — restart cooldown active (~${remaining}s remaining)"
                cooldown_logged=1
            elif [ "$remaining" -le $(( RESTART_COOLDOWN / 2 )) ] && [ "$cooldown_logged" -eq 1 ]; then
                logw "Restart cooldown: ${remaining}s remaining"
                cooldown_logged=2
            elif [ "$remaining" -le 5 ] && [ "$cooldown_logged" -ge 1 ]; then
                logw "Restart cooldown almost expired: ${remaining}s remaining"
                cooldown_logged=3
            fi
            continue
        fi
        # Cooldown expired — fall through to restart block below.
    fi

    # 1. Daemon must be running.
    if ! systemctl is-active --quiet timed 2>/dev/null; then
        # timed is dead (OOM killed, segfault, etc.) — restart it directly.
        # Don't rely on systemd's Restart= policy; the watchdog owns recovery.
        now=$(date +%s)
        since_last=$(( now - last_restart_ts ))
        if [ "$since_last" -ge "$RESTART_COOLDOWN" ]; then
            total_restarts=$(( total_restarts + 1 ))
            if [ "$DRY_RUN" -eq 1 ]; then
                log "DRY-RUN: timed is not active — would restart (restart #${total_restarts})"
            else
                log "💀 timed is not active — restarting (restart #${total_restarts})"
                if systemctl restart timed; then
                    log "✅ systemctl restart timed succeeded"
                else
                    loge "systemctl restart timed FAILED (exit $?)"
                fi
            fi
            last_restart_ts=$now
            cooldown_logged=0
        else
            remaining=$(( RESTART_COOLDOWN - since_last ))
            logw "timed is not active — restart cooldown active (${remaining}s remaining)"
        fi
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

    # 2b. Post-restart grace: after each restart give the daemon time to initialize
    #     before we start polling.  Emits a single log line on entry and skips RPC
    #     calls until the grace window expires, avoiding silent rpc_busy_streak
    #     burn-down and false "unresponsive" log noise.
    if [ "$last_restart_ts" -gt 0 ]; then
        since_restart=$(( $(date +%s) - last_restart_ts ))
        if [ "$since_restart" -lt "$POST_RESTART_GRACE" ]; then
            remaining=$(( POST_RESTART_GRACE - since_restart ))
            # Log only once when entering grace (first poll after restart)
            if [ "$remaining" -ge $(( POST_RESTART_GRACE - POLL_INTERVAL - 1 )) ]; then
                log "⏸️ Post-restart grace: waiting ${POST_RESTART_GRACE}s for daemon to initialize"
            fi
            continue
        fi
    fi

    # 3. Query masternodestatus — with hard timeout so a dead RPC socket
    #    doesn't stall each check for 60+ seconds.
    status_json=$(timeout "$RPC_TIMEOUT" $CLI_CMD masternodestatus 2>/dev/null) || status_json=""
    if [ -z "$status_json" ]; then
        # RPC timed out or returned nothing.  Before treating as a hard failure,
        # check whether the daemon is still writing to its log.  A node busy with
        # fork resolution or a reconnection storm can starve its tokio RPC thread
        # for up to ~90s while still alive and registered.
        if daemon_recently_active "$DAEMON_ACTIVE_SECS"; then
            # Daemon is alive — any fail_streak counts from the "not logging" path
            # were false positives caused by a brief journald lull.  Clear them now
            # so they don't combine with a later busy_streak escalation and trigger
            # an unwarranted restart.
            if [ "$fail_streak" -gt 0 ]; then
                log "Daemon is logging again — clearing stale fail_streak (was ${fail_streak}) to prevent false-positive restart"
                fail_streak=0
            fi
            rpc_busy_streak=$(( rpc_busy_streak + 1 ))
            logw "⏳ RPC timeout — daemon is alive and logging (busy_streak: ${rpc_busy_streak}/${RPC_BUSY_MAX}); NOT counting as de-registration"
            # Dump diagnostics on the first poll of every stall, then every 5 polls.
            if [ "$rpc_busy_streak" -eq 1 ] || [ $(( rpc_busy_streak % 5 )) -eq 0 ]; then
                log_stall_diagnostics "$rpc_busy_streak"
            fi
            # Sustained RPC unresponsiveness while logging is conclusive on its own —
            # jump straight to FAIL_THRESHOLD so the restart triggers immediately
            # instead of waiting for N more escalation cycles (~minutes of delay).
            if [ "$rpc_busy_streak" -ge "$RPC_BUSY_MAX" ]; then
                logw "🔴 Daemon has been RPC-unresponsive for $((rpc_busy_streak * POLL_INTERVAL))s while logging — triggering restart"
                fail_streak=$FAIL_THRESHOLD
                rpc_busy_streak=0
            fi
        else
            logw "RPC call failed — daemon has NOT logged in ${DAEMON_ACTIVE_SECS}s (streak: $((fail_streak + 1))/$FAIL_THRESHOLD)"
            fail_streak=$(( fail_streak + 1 ))
            rpc_busy_streak=0
        fi
    else
        rpc_busy_streak=0
        # Parse the "status" field.  Uses jq if available, falls back to grep.
        if command -v jq &>/dev/null; then
            mn_status=$(echo "$status_json" | jq -r '.status // "unknown"' 2>/dev/null)
            is_active=$(echo "$status_json"  | jq -r '.is_active // false'  2>/dev/null)
        else
            mn_status=$(echo "$status_json" | grep -oP '"status"\s*:\s*"\K[^"]+' | head -1)
            is_active=$(echo "$status_json" | grep -oP '"is_active"\s*:\s*\K(true|false)' | head -1)
        fi

        if [ "$mn_status" = "active" ] && [ "$is_active" = "true" ]; then
            # Healthy — reset both streaks.
            if [ "$fail_streak" -gt 0 ] || [ "$rpc_busy_streak" -gt 0 ]; then
                log "Masternode active again — clearing failure streak"
            fi
            fail_streak=0
            rpc_busy_streak=0
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
            continue
        fi

        total_restarts=$(( total_restarts + 1 ))

        if [ "$DRY_RUN" -eq 1 ]; then
            log "DRY-RUN: would run 'systemctl restart timed' (restart #${total_restarts})"
        else
            log "🔁 De-registration confirmed after ${fail_streak} consecutive checks — restarting timed (restart #${total_restarts})"
            if systemctl restart timed; then
                log "✅ systemctl restart timed succeeded"
            else
                loge "systemctl restart timed FAILED (exit $?)"
            fi
        fi

        last_restart_ts=$now
        fail_streak=0
        cooldown_logged=0
    fi
done
