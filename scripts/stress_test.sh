#!/bin/bash
# TIME Coin Network Stress Test
# Sends transactions at increasing rates to measure finalization performance
# and find the network saturation point.
#
# Usage:
#   bash scripts/stress_test.sh [OPTIONS]
#
# Options:
#   -n, --total        Total transactions to send (default: auto from --per-step)
#   -s, --start-rate   Starting TPS rate (default: 5)
#   -m, --max-rate     Maximum TPS rate to ramp to (default: 50)
#   -r, --ramp-step    TPS increase per ramp interval (default: 5)
#   -i, --ramp-interval Seconds between rate increases (default: 30, ignored with --per-step)
#   -p, --per-step     TXs to send at each rate level before stepping up (default: 20)
#                       When set, ignores --ramp-interval and auto-calculates --total
#   -a, --amount       TIME per transaction (default: 0.001)
#   -t, --timeout      Finality poll timeout per TX in seconds (default: 60)
#   -o, --output       CSV output file (default: stress_results_<timestamp>.csv)
#   --testnet          Use testnet (passes --testnet to time-cli)
#   --no-early-stop    Disable early termination on saturation
#   -h, --help         Show this help
#
# Example:
#   bash scripts/stress_test.sh --testnet -p 30 -s 5 -m 100 -r 5

set -euo pipefail

# ── Defaults ──────────────────────────────────────────────────────────────────
TOTAL_TX=0
START_RATE=5
MAX_RATE=50
RAMP_STEP=5
RAMP_INTERVAL=30
PER_STEP=20
AMOUNT="0.001"
FINALITY_TIMEOUT=60
OUTPUT=""
CLI_FLAGS=""
EARLY_STOP=1

# ── Colors ────────────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

log_info()    { echo -e "${BLUE}ℹ️  $1${NC}"; }
log_success() { echo -e "${GREEN}✅ $1${NC}"; }
log_warning() { echo -e "${YELLOW}⚠️  $1${NC}"; }
log_error()   { echo -e "${RED}❌ $1${NC}"; }
log_header()  { echo -e "${BOLD}${CYAN}═══ $1 ═══${NC}"; }

# ── Parse Arguments ───────────────────────────────────────────────────────────
usage() {
    sed -n '/^# Usage:/,/^$/p' "$0" | sed 's/^# //'
    sed -n '/^# Options:/,/^$/p' "$0" | sed 's/^# //'
    sed -n '/^# Example:/,/^$/p' "$0" | sed 's/^# //'
    exit 0
}

while [[ $# -gt 0 ]]; do
    case $1 in
        -n|--total)         TOTAL_TX="$2";       shift 2 ;;
        -s|--start-rate)    START_RATE="$2";      shift 2 ;;
        -m|--max-rate)      MAX_RATE="$2";        shift 2 ;;
        -r|--ramp-step)     RAMP_STEP="$2";       shift 2 ;;
        -i|--ramp-interval) RAMP_INTERVAL="$2";   shift 2 ;;
        -p|--per-step)      PER_STEP="$2";        shift 2 ;;
        -a|--amount)        AMOUNT="$2";          shift 2 ;;
        -t|--timeout)       FINALITY_TIMEOUT="$2"; shift 2 ;;
        -o|--output)        OUTPUT="$2";          shift 2 ;;
        --testnet)          CLI_FLAGS="--testnet"; shift ;;
        --no-early-stop)    EARLY_STOP=0;         shift ;;
        -h|--help)          usage ;;
        *) log_error "Unknown option: $1"; usage ;;
    esac
done

# Auto-calculate total if not explicitly set
if [ "$TOTAL_TX" -eq 0 ]; then
    NUM_STEPS=$(( (MAX_RATE - START_RATE) / RAMP_STEP + 1 ))
    TOTAL_TX=$(( NUM_STEPS * PER_STEP ))
fi

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
if [ -z "$OUTPUT" ]; then
    OUTPUT="stress_results_${TIMESTAMP}.csv"
fi

# ── Find CLI ──────────────────────────────────────────────────────────────────
if [ -n "${CLI_PATH:-}" ]; then
    CLI_CMD="$CLI_PATH"
elif command -v time-cli &> /dev/null; then
    CLI_CMD="time-cli"
elif [ -x "./target/release/time-cli" ]; then
    CLI_CMD="./target/release/time-cli"
elif [ -x "./time-cli" ]; then
    CLI_CMD="./time-cli"
else
    log_error "time-cli not found. Set CLI_PATH or ensure it's in PATH."
    exit 1
fi

# Append network flag if specified
if [ -n "$CLI_FLAGS" ]; then
    CLI_CMD="$CLI_CMD $CLI_FLAGS"
fi

# ── Prerequisites ─────────────────────────────────────────────────────────────
log_header "TIME Coin Network Stress Test"
echo ""
log_info "CLI:            $CLI_CMD"
log_info "Total TX:       $TOTAL_TX"
log_info "Rate ramp:      ${START_RATE} → ${MAX_RATE} TPS (step ${RAMP_STEP}, ${PER_STEP} TX/step)"
log_info "Amount/TX:      $AMOUNT TIME"
log_info "Finality timeout: ${FINALITY_TIMEOUT}s"
log_info "Early stop:     $([ "$EARLY_STOP" -eq 1 ] && echo "enabled" || echo "disabled")"
log_info "Output:         $OUTPUT"
echo ""

# Check daemon
if ! $CLI_CMD getblockchaininfo >/dev/null 2>&1; then
    log_error "Cannot connect to timed daemon"
    exit 1
fi
log_success "Daemon reachable"

# Check balance
BALANCE_JSON=$($CLI_CMD getbalance 2>&1)
AVAILABLE=$(echo "$BALANCE_JSON" | jq -r '.available // 0')
NEEDED=$(awk -v n="$TOTAL_TX" -v a="$AMOUNT" 'BEGIN { printf "%.8f", n * a * 1.002 }')
HAS_ENOUGH=$(awk -v a="$AVAILABLE" -v n="$NEEDED" 'BEGIN { print (a >= n) ? 1 : 0 }')

if [ "$HAS_ENOUGH" -eq 0 ]; then
    log_error "Insufficient balance: have $AVAILABLE TIME, need ~$NEEDED TIME ($TOTAL_TX × $AMOUNT + fees)"
    exit 1
fi
log_success "Balance OK: $AVAILABLE TIME available (need ~$NEEDED)"

# Get recipient address
MN_JSON=$($CLI_CMD masternodelist 2>&1)
CONNECTED_MN=$(echo "$MN_JSON" | jq -r '[.masternodes[]? | select(.is_connected == true)] | length')

if [ "$CONNECTED_MN" -eq 0 ]; then
    log_error "No connected masternodes"
    exit 1
fi

OUR_ADDRESS=$($CLI_CMD masternodestatus 2>/dev/null | jq -r '.reward_address // empty')
RECIPIENT=$(echo "$MN_JSON" | jq -r --arg ours "$OUR_ADDRESS" \
    '.masternodes[]? | select(.is_connected == true and .wallet_address != $ours) | .wallet_address' | head -n 1)

if [ -z "$RECIPIENT" ] || [ "$RECIPIENT" = "null" ]; then
    RECIPIENT=$(echo "$MN_JSON" | jq -r '.masternodes[]? | select(.is_connected == true) | .wallet_address' | head -n 1)
fi

log_success "Recipient: $RECIPIENT"
log_success "Connected masternodes: $CONNECTED_MN"
echo ""

# ── CSV Header ────────────────────────────────────────────────────────────────
echo "tx_seq,txid,target_tps,actual_tps,send_time_unix,send_latency_ms,finality_time_ms,finalized,votes,accumulated_weight,confirmations,error" > "$OUTPUT"

# ── Tracking Arrays ──────────────────────────────────────────────────────────
declare -a TX_IDS=()
declare -a TX_SEND_TIMES=()
declare -a TX_SEND_LATENCIES=()
declare -a TX_TARGET_RATES=()
declare -a TX_SEQS=()

# Stats counters
SENT=0
SEND_FAILURES=0
FINALIZED_COUNT=0
FINALITY_FAILURES=0
CURRENT_RATE=$START_RATE
RAMP_START=$(date +%s)

# ── Millisecond timer ────────────────────────────────────────────────────────
ms_now() {
    # Returns milliseconds since epoch
    if date +%s%3N >/dev/null 2>&1; then
        date +%s%3N
    else
        # Fallback: seconds * 1000
        echo $(( $(date +%s) * 1000 ))
    fi
}

# ── Send Phase ────────────────────────────────────────────────────────────────
log_header "Phase 1: Sending $TOTAL_TX Transactions (Ramping ${START_RATE}→${MAX_RATE} TPS)"
echo ""

PHASE1_START=$(ms_now)
STEP_TX_COUNT=0
STEP_SEND_FAILURES=0
STOPPED_EARLY=0

for (( i=1; i<=TOTAL_TX; i++ )); do
    # Count-based ramp: step up after PER_STEP transactions at current rate
    if [ "$STEP_TX_COUNT" -ge "$PER_STEP" ]; then
        # Early stop: if >50% of this step's TXs failed to send, network is saturated
        if [ "$EARLY_STOP" -eq 1 ] && [ "$STEP_SEND_FAILURES" -gt $(( PER_STEP / 2 )) ]; then
            echo ""
            log_warning "Early stop: ${STEP_SEND_FAILURES}/${PER_STEP} send failures at ${CURRENT_RATE} TPS"
            STOPPED_EARLY=1
            break
        fi

        OLD_RATE=$CURRENT_RATE
        CURRENT_RATE=$(( CURRENT_RATE + RAMP_STEP ))
        if [ "$CURRENT_RATE" -gt "$MAX_RATE" ]; then
            CURRENT_RATE=$MAX_RATE
        fi
        if [ "$CURRENT_RATE" -ne "$OLD_RATE" ]; then
            echo ""
            log_info "Rate increased: ${OLD_RATE} → ${CURRENT_RATE} TPS (sent ${STEP_TX_COUNT} @ ${OLD_RATE}, ${STEP_SEND_FAILURES} failures)"
        fi
        STEP_TX_COUNT=0
        STEP_SEND_FAILURES=0
    fi

    # Rate limiting: sleep to maintain target TPS
    if [ "$STEP_TX_COUNT" -gt 0 ] && [ "$CURRENT_RATE" -gt 0 ]; then
        TARGET_INTERVAL_MS=$(( 1000 / CURRENT_RATE ))
        SLEEP_S=$(awk -v ms="$TARGET_INTERVAL_MS" 'BEGIN { printf "%.3f", ms / 1000.0 }')
        sleep "$SLEEP_S"
    fi

    # Send transaction
    SEND_START=$(ms_now)
    SEND_RESULT=$($CLI_CMD sendtoaddress "$RECIPIENT" "$AMOUNT" --nowait 2>&1) || true
    SEND_END=$(ms_now)
    SEND_LATENCY=$(( SEND_END - SEND_START ))

    TXID=$(echo "$SEND_RESULT" | tr -d '"' | tr -d "'" | tr -d '\n' | tr -d ' ' | grep -oE '[a-f0-9]{64}' || true)

    if [ -z "$TXID" ]; then
        SEND_FAILURES=$(( SEND_FAILURES + 1 ))
        STEP_SEND_FAILURES=$(( STEP_SEND_FAILURES + 1 ))
        echo "$i,,${CURRENT_RATE},,${SEND_START},${SEND_LATENCY},,false,,,send_failed: $(echo "$SEND_RESULT" | tr ',' ';' | tr '\n' ' ')" >> "$OUTPUT"
        echo -ne "\r  TX $i/$TOTAL_TX @ ${CURRENT_RATE} TPS — ❌ SEND FAILED (${SEND_LATENCY}ms)            "
        STEP_TX_COUNT=$(( STEP_TX_COUNT + 1 ))
        continue
    fi

    SENT=$(( SENT + 1 ))
    TX_IDS+=("$TXID")
    TX_SEND_TIMES+=("$SEND_START")
    TX_SEND_LATENCIES+=("$SEND_LATENCY")
    TX_TARGET_RATES+=("$CURRENT_RATE")
    TX_SEQS+=("$i")

    echo -ne "\r  TX $i/$TOTAL_TX @ ${CURRENT_RATE} TPS — ${TXID:0:12}... (${SEND_LATENCY}ms)            "
    STEP_TX_COUNT=$(( STEP_TX_COUNT + 1 ))
done

PHASE1_END=$(ms_now)
PHASE1_DURATION=$(( PHASE1_END - PHASE1_START ))
echo ""
echo ""
log_success "Sent $SENT/$TOTAL_TX transactions in $(( PHASE1_DURATION / 1000 ))s ($SEND_FAILURES failures)"
ACTUAL_OVERALL_TPS=$(awk -v s="$SENT" -v d="$PHASE1_DURATION" 'BEGIN { if (d > 0) printf "%.2f", s / (d / 1000.0); else print "0" }')
log_info "Effective send rate: ${ACTUAL_OVERALL_TPS} TPS"
echo ""

# ── Finality Phase ────────────────────────────────────────────────────────────
log_header "Phase 2: Polling Finality for $SENT Transactions"
echo ""

PHASE2_START=$(ms_now)
CONSECUTIVE_TIMEOUTS=0

# Build associative arrays for batch tracking
declare -A FINALITY_RESULT   # txid -> "true" or "false"
declare -A FINALITY_VOTES    # txid -> votes count
declare -A FINALITY_WEIGHT   # txid -> accumulated weight
declare -A FINALITY_CONFS    # txid -> confirmations
declare -A FINALITY_ERROR    # txid -> error string
declare -A FINALITY_DETECT   # txid -> ms timestamp when finality detected

for txid in "${TX_IDS[@]}"; do
    FINALITY_RESULT["$txid"]="false"
done

REMAINING=${#TX_IDS[@]}
POLL_DELAY="0.05"        # Start at 50ms
MAX_POLL_DELAY="0.5"     # Cap at 500ms
BATCH_SIZE=50            # TXIDs per batch RPC call
GLOBAL_DEADLINE=$(( $(date +%s) + FINALITY_TIMEOUT ))

while [ "$REMAINING" -gt 0 ] && [ "$(date +%s)" -lt "$GLOBAL_DEADLINE" ]; do
    # Collect un-finalized txids into batches
    UNFIN_TXIDS=()
    for txid in "${TX_IDS[@]}"; do
        if [ "${FINALITY_RESULT[$txid]}" = "false" ]; then
            UNFIN_TXIDS+=("$txid")
        fi
    done
    REMAINING=${#UNFIN_TXIDS[@]}
    [ "$REMAINING" -eq 0 ] && break

    # Try batch RPC first (gettransactions), fall back to single queries
    for (( b=0; b<${#UNFIN_TXIDS[@]}; b+=BATCH_SIZE )); do
        BATCH=("${UNFIN_TXIDS[@]:$b:$BATCH_SIZE}")
        BATCH_JSON=$(printf '"%s",' "${BATCH[@]}")
        BATCH_JSON="[${BATCH_JSON%,}]"

        BATCH_RESULT=$($CLI_CMD gettransactions "$BATCH_JSON" 2>&1) || BATCH_RESULT=""

        if echo "$BATCH_RESULT" | jq -e '.transactions' >/dev/null 2>&1; then
            # Batch succeeded — parse results
            for row in $(echo "$BATCH_RESULT" | jq -c '.transactions[]'); do
                TXID=$(echo "$row" | jq -r '.txid')
                IS_FIN=$(echo "$row" | jq -r '.finalized // false')
                HAS_TP=$(echo "$row" | jq -r '.timeproof // null')
                CONF=$(echo "$row" | jq -r '.confirmations // 0')

                if [ "$IS_FIN" = "true" ] || [ "$HAS_TP" != "null" ] || [ "$CONF" -gt 0 ] 2>/dev/null; then
                    FINALITY_RESULT["$TXID"]="true"
                    FINALITY_DETECT["$TXID"]=$(ms_now)
                    FINALITY_CONFS["$TXID"]="$CONF"
                    if [ "$HAS_TP" != "null" ]; then
                        FINALITY_VOTES["$TXID"]=$(echo "$row" | jq -r '.timeproof.votes // 0')
                        FINALITY_WEIGHT["$TXID"]=$(echo "$row" | jq -r '.timeproof.accumulated_weight // 0')
                    fi
                fi
            done
        else
            # Batch not supported — fall back to single queries
            for TXID in "${BATCH[@]}"; do
                TX_INFO=$($CLI_CMD gettransaction "$TXID" 2>&1) || true
                if echo "$TX_INFO" | jq -e . >/dev/null 2>&1; then
                    IS_FIN=$(echo "$TX_INFO" | jq -r '.finalized // false')
                    HAS_TP=$(echo "$TX_INFO" | jq -r '.timeproof // null')
                    CONF=$(echo "$TX_INFO" | jq -r '.confirmations // 0')

                    if [ "$IS_FIN" = "true" ] || [ "$HAS_TP" != "null" ] || [ "$CONF" -gt 0 ] 2>/dev/null; then
                        FINALITY_RESULT["$TXID"]="true"
                        FINALITY_DETECT["$TXID"]=$(ms_now)
                        FINALITY_CONFS["$TXID"]="$CONF"
                        if [ "$HAS_TP" != "null" ]; then
                            FINALITY_VOTES["$TXID"]=$(echo "$TX_INFO" | jq -r '.timeproof.votes // 0')
                            FINALITY_WEIGHT["$TXID"]=$(echo "$TX_INFO" | jq -r '.timeproof.accumulated_weight // 0')
                        fi
                    fi
                fi
            done
        fi
    done

    # Count finalized
    NEW_REMAINING=0
    FINALIZED_COUNT=0
    for txid in "${TX_IDS[@]}"; do
        if [ "${FINALITY_RESULT[$txid]}" = "true" ]; then
            FINALIZED_COUNT=$(( FINALIZED_COUNT + 1 ))
        else
            NEW_REMAINING=$(( NEW_REMAINING + 1 ))
        fi
    done
    REMAINING=$NEW_REMAINING

    echo -ne "\r  [${FINALIZED_COUNT}/${SENT}] finalized, ${REMAINING} remaining (poll interval: ${POLL_DELAY}s)        "

    # Exponential backoff: increase delay by 1.5x, cap at MAX_POLL_DELAY
    if [ "$REMAINING" -gt 0 ]; then
        sleep "$POLL_DELAY"
        POLL_DELAY=$(awk -v d="$POLL_DELAY" -v m="$MAX_POLL_DELAY" 'BEGIN { nd=d*1.5; if (nd>m) nd=m; printf "%.3f", nd }')
    fi
done

# Write results for each TX
FINALITY_FAILURES=0
for (( idx=0; idx<${#TX_IDS[@]}; idx++ )); do
    TXID="${TX_IDS[$idx]}"
    SEND_TIME="${TX_SEND_TIMES[$idx]}"
    SEND_LATENCY="${TX_SEND_LATENCIES[$idx]}"
    TARGET_RATE="${TX_TARGET_RATES[$idx]}"
    SEQ="${TX_SEQS[$idx]}"
    ACTUAL_TPS="$TARGET_RATE"
    VOTES="${FINALITY_VOTES[$TXID]:-}"
    WEIGHT="${FINALITY_WEIGHT[$TXID]:-}"
    CONFS="${FINALITY_CONFS[$TXID]:-}"
    ERROR="${FINALITY_ERROR[$TXID]:-}"
    FINALIZED="${FINALITY_RESULT[$TXID]}"

    if [ "$FINALIZED" = "true" ]; then
        DETECT_TIME="${FINALITY_DETECT[$TXID]}"
        FINALITY_TIME=$(( DETECT_TIME - SEND_TIME ))
    else
        FINALITY_FAILURES=$(( FINALITY_FAILURES + 1 ))
        FINALITY_TIME=""
        ERROR="timeout"
    fi

    echo "${SEQ},${TXID},${TARGET_RATE},${ACTUAL_TPS},${SEND_TIME},${SEND_LATENCY},${FINALITY_TIME},${FINALIZED},${VOTES},${WEIGHT},${CONFS},${ERROR}" >> "$OUTPUT"
done

# Show last TX result
LAST_TXID="${TX_IDS[$((SENT-1))]}"
if [ "${FINALITY_RESULT[$LAST_TXID]}" = "true" ]; then
    LAST_FT=$(( ${FINALITY_DETECT[$LAST_TXID]} - ${TX_SEND_TIMES[$((SENT-1))]} ))
    echo -ne "\r  [${FINALIZED_COUNT}/${SENT}] ${LAST_TXID:0:12}... ✅ ${LAST_FT}ms            "
fi

PHASE2_END=$(ms_now)
echo ""
echo ""

# ── Statistics ────────────────────────────────────────────────────────────────
log_header "Results Summary"
echo ""

# Parse CSV for statistics (skip header)
FINALITY_TIMES=$(awk -F',' 'NR>1 && $8=="true" && $7!="" { print $7 }' "$OUTPUT")
SEND_LATENCIES_ALL=$(awk -F',' 'NR>1 && $6!="" { print $6 }' "$OUTPUT")

if [ -n "$FINALITY_TIMES" ]; then
    STATS=$(echo "$FINALITY_TIMES" | awk '
    BEGIN { min=999999999; max=0; sum=0; n=0; }
    {
        n++; sum+=$1;
        if ($1 < min) min=$1;
        if ($1 > max) max=$1;
        vals[n]=$1;
    }
    END {
        if (n == 0) { print "0 0 0 0 0"; exit }
        avg = sum/n;

        # Sort for percentiles
        for (i=1; i<=n; i++)
            for (j=i+1; j<=n; j++)
                if (vals[i] > vals[j]) { t=vals[i]; vals[i]=vals[j]; vals[j]=t; }

        p50_idx = int(n * 0.50); if (p50_idx < 1) p50_idx=1;
        p95_idx = int(n * 0.95); if (p95_idx < 1) p95_idx=1;
        p99_idx = int(n * 0.99); if (p99_idx < 1) p99_idx=1;

        printf "%d %d %.0f %d %d %d %d\n", min, max, avg, vals[p50_idx], vals[p95_idx], vals[p99_idx], n;
    }')

    F_MIN=$(echo "$STATS" | awk '{print $1}')
    F_MAX=$(echo "$STATS" | awk '{print $2}')
    F_AVG=$(echo "$STATS" | awk '{print $3}')
    F_P50=$(echo "$STATS" | awk '{print $4}')
    F_P95=$(echo "$STATS" | awk '{print $5}')
    F_P99=$(echo "$STATS" | awk '{print $6}')
fi

SEND_STATS=$(echo "$SEND_LATENCIES_ALL" | awk '
BEGIN { min=999999999; max=0; sum=0; n=0; }
{
    n++; sum+=$1;
    if ($1 < min) min=$1;
    if ($1 > max) max=$1;
}
END {
    if (n == 0) { print "0 0 0"; exit }
    printf "%d %d %.0f\n", min, max, sum/n;
}')

S_MIN=$(echo "$SEND_STATS" | awk '{print $1}')
S_MAX=$(echo "$SEND_STATS" | awk '{print $2}')
S_AVG=$(echo "$SEND_STATS" | awk '{print $3}')

# Per-rate breakdown
log_info "Per-Rate Breakdown:"
echo ""
printf "  ${BOLD}%-8s  %-6s  %-10s  %-10s  %-10s  %-6s${NC}\n" "TPS" "Count" "Avg(ms)" "P50(ms)" "P95(ms)" "Fails"
echo "  ──────  ──────  ──────────  ──────────  ──────────  ──────"

awk -F',' 'NR>1 {
    rate=$3; fin=$8; ft=$7;
    count[rate]++;
    if (fin == "true" && ft != "") {
        fin_count[rate]++;
        fin_sum[rate] += ft;
        fin_vals[rate][fin_count[rate]] = ft;
    } else {
        fail_count[rate]++;
    }
}
END {
    n = asorti(count, sorted, "@ind_num_asc");
    for (i=1; i<=n; i++) {
        r = sorted[i];
        c = count[r];
        f = fail_count[r]+0;
        fc = fin_count[r]+0;
        if (fc > 0) {
            avg = fin_sum[r] / fc;
            # Collect values for percentiles
            split("", v);
            k = 0;
            for (j=1; j<=fc; j++) v[++k] = fin_vals[r][j];
            # Sort
            for (a=1; a<=k; a++)
                for (b=a+1; b<=k; b++)
                    if (v[a]+0 > v[b]+0) { t=v[a]; v[a]=v[b]; v[b]=t; }
            p50 = v[int(k*0.50) < 1 ? 1 : int(k*0.50)];
            p95 = v[int(k*0.95) < 1 ? 1 : int(k*0.95)];
            printf "  %-8s  %-6d  %-10.0f  %-10d  %-10d  %-6d\n", r, c, avg, p50, p95, f;
        } else {
            printf "  %-8s  %-6d  %-10s  %-10s  %-10s  %-6d\n", r, c, "N/A", "N/A", "N/A", f;
        }
    }
}' "$OUTPUT"

echo ""
echo "══════════════════════════════════════════════════════════════"
echo "                     STRESS TEST SUMMARY"
echo "══════════════════════════════════════════════════════════════"
echo "  Transactions sent:      $SENT / $TOTAL_TX"
echo "  Send failures:          $SEND_FAILURES"
echo "  Finalized:              $FINALIZED_COUNT / $SENT"
echo "  Finality timeouts:      $FINALITY_FAILURES"
echo "  Peak TPS attempted:     ${CURRENT_RATE} TPS"
echo "  Effective send rate:    ${ACTUAL_OVERALL_TPS} TPS"
echo "══════════════════════════════════════════════════════════════"
echo "                   SEND LATENCY (RPC → TXID)"
echo "══════════════════════════════════════════════════════════════"
echo "  Min:                    ${S_MIN}ms"
echo "  Max:                    ${S_MAX}ms"
echo "  Avg:                    ${S_AVG}ms"
echo "══════════════════════════════════════════════════════════════"

if [ -n "${F_MIN:-}" ]; then
echo "                FINALITY TIME (Send → Confirmed)"
echo "══════════════════════════════════════════════════════════════"
echo "  Min:                    ${F_MIN}ms"
echo "  Max:                    ${F_MAX}ms"
echo "  Avg:                    ${F_AVG}ms"
echo "  P50 (median):           ${F_P50}ms"
echo "  P95:                    ${F_P95}ms"
echo "  P99:                    ${F_P99}ms"
echo "══════════════════════════════════════════════════════════════"
fi

echo "  CSV output:             $OUTPUT"
echo "══════════════════════════════════════════════════════════════"
echo ""

# Final verdict
if [ "$STOPPED_EARLY" -eq 1 ]; then
    log_error "Network saturated during send phase at ${CURRENT_RATE} TPS"
elif [ "$FINALITY_FAILURES" -gt 0 ]; then
    FAIL_RATE=$(awk -v f="$FINALITY_FAILURES" -v s="$SENT" 'BEGIN { printf "%.1f", f/s*100 }')
    if [ "$FINALITY_FAILURES" -gt $(( SENT / 2 )) ]; then
        log_error "Network saturated: ${FAIL_RATE}% finality failures"
    else
        log_warning "Partial degradation: ${FAIL_RATE}% finality failures"
    fi

    # Find the rate where failures started
    SATURATION_RATE=$(awk -F',' 'NR>1 && $8!="true" { print $3 }' "$OUTPUT" | sort -n | head -1)
    if [ -n "$SATURATION_RATE" ]; then
        log_info "First failures appeared at ${SATURATION_RATE} TPS"
    fi

    # Find the last rate with 100% finalization
    LAST_GOOD_RATE=$(awk -F',' 'NR>1 {
        rate=$3; fin=$8;
        total[rate]++;
        if (fin == "true") good[rate]++;
    } END {
        best=0;
        for (r in total) {
            if (good[r]+0 == total[r] && r+0 > best) best=r+0;
        }
        print best;
    }' "$OUTPUT")
    if [ "$LAST_GOOD_RATE" -gt 0 ] 2>/dev/null; then
        log_success "Last clean rate: ${LAST_GOOD_RATE} TPS (100% finalization)"
    fi
else
    log_success "All transactions finalized — network handled peak ${CURRENT_RATE} TPS"
fi

echo ""
log_info "Analyze with: column -s, -t < $OUTPUT"
log_info "Graph in Excel/Sheets: Import $OUTPUT, chart finality_time_ms vs target_tps"

exit 0
