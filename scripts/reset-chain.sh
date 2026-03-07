#!/usr/bin/env bash
# reset-chain.sh — Delete all blocks except genesis, then restart the daemon.
#
# USE CASE: Node is hopelessly behind, corrupt, or you want a clean resync
# from peers without losing wallet data, config, or masternode registration.
#
# WHAT IS DELETED:
#   - All blocks (except genesis — recreated automatically on first start)
#   - Transaction index (rebuilt on resync)
#   - UTXO state (rebuilt from blocks during resync)
#   - Chain work index
#
# WHAT IS PRESERVED:
#   - time.conf, masternode.conf
#   - wallet.json
#   - .cookie (RPC auth)
#   - Peer database (speeds up reconnection)
#   - AI state
#
# USAGE:
#   ./scripts/reset-chain.sh [--testnet] [--mainnet] [--yes]
#
#   --testnet    Target testnet data dir (default)
#   --mainnet    Target mainnet data dir
#   --yes        Skip confirmation prompt

set -euo pipefail

# ── defaults ─────────────────────────────────────────────────────────────────
NETWORK="testnet"
SKIP_CONFIRM=false

# ── argument parsing ──────────────────────────────────────────────────────────
for arg in "$@"; do
    case "$arg" in
        --testnet)  NETWORK="testnet" ;;
        --mainnet)  NETWORK="mainnet" ;;
        --yes|-y)   SKIP_CONFIRM=true ;;
        --help|-h)
            sed -n '2,25p' "$0" | sed 's/^# \?//'
            exit 0
            ;;
        *)
            echo "Unknown argument: $arg"
            exit 1
            ;;
    esac
done

# ── resolve data directory ────────────────────────────────────────────────────
if [[ "$NETWORK" == "testnet" ]]; then
    DATA_DIR="${HOME}/.timecoin/testnet"
else
    DATA_DIR="${HOME}/.timecoin"
fi

DB_DIR="${DATA_DIR}/db"
BLOCKS_DB="${DB_DIR}/blocks"
TXINDEX_DB="${DB_DIR}/txindex"

# ── check data dir exists ─────────────────────────────────────────────────────
if [[ ! -d "$DATA_DIR" ]]; then
    echo "❌ Data directory not found: $DATA_DIR"
    echo "   Is timed configured for ${NETWORK}?"
    exit 1
fi

if [[ ! -d "$BLOCKS_DB" ]]; then
    echo "ℹ️  No block database found at $BLOCKS_DB — nothing to reset."
    exit 0
fi

# ── show what will be deleted ─────────────────────────────────────────────────
BLOCKS_SIZE=$(du -sh "$BLOCKS_DB" 2>/dev/null | cut -f1 || echo "unknown")
TXINDEX_SIZE="none"
if [[ -d "$TXINDEX_DB" ]]; then
    TXINDEX_SIZE=$(du -sh "$TXINDEX_DB" 2>/dev/null | cut -f1 || echo "unknown")
fi

echo ""
echo "🔗 TIME Coin — Chain Reset (${NETWORK})"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "   Data dir   : $DATA_DIR"
echo "   Block DB   : $BLOCKS_DB ($BLOCKS_SIZE)"
echo "   TX index   : $TXINDEX_DB ($TXINDEX_SIZE)"
echo ""
echo "   Will DELETE : block database, transaction index, UTXO state"
echo "   Will KEEP   : time.conf, masternode.conf, wallet.json, peers"
echo ""
echo "⚠️  The node will resync from scratch on next start."
echo "   This may take several minutes depending on network height."
echo ""

# ── confirm ───────────────────────────────────────────────────────────────────
if [[ "$SKIP_CONFIRM" == false ]]; then
    read -r -p "Type 'reset' to confirm, or Ctrl-C to cancel: " CONFIRM
    if [[ "$CONFIRM" != "reset" ]]; then
        echo "Aborted."
        exit 0
    fi
fi

# ── stop the daemon if running ────────────────────────────────────────────────
TIMED_BIN=""
for candidate in "./target/release/timed" "/usr/local/bin/timed" "$(which timed 2>/dev/null)"; do
    if [[ -x "$candidate" ]]; then
        TIMED_BIN="$candidate"
        break
    fi
done

CLI_BIN=""
for candidate in "./target/release/time-cli" "/usr/local/bin/time-cli" "$(which time-cli 2>/dev/null)"; do
    if [[ -x "$candidate" ]]; then
        CLI_BIN="$candidate"
        break
    fi
done

RPC_PORT=24101
[[ "$NETWORK" == "mainnet" ]] && RPC_PORT=24001

DAEMON_WAS_RUNNING=false
if [[ -n "$CLI_BIN" ]]; then
    if "$CLI_BIN" --rpcport "$RPC_PORT" getblockcount &>/dev/null; then
        echo "🛑 Stopping timed daemon..."
        "$CLI_BIN" --rpcport "$RPC_PORT" stop &>/dev/null || true
        # Wait up to 15s for daemon to exit
        for i in $(seq 1 15); do
            sleep 1
            if ! "$CLI_BIN" --rpcport "$RPC_PORT" getblockcount &>/dev/null; then
                break
            fi
        done
        DAEMON_WAS_RUNNING=true
        echo "   Daemon stopped."
    fi
fi

# ── delete chain data ─────────────────────────────────────────────────────────
echo ""
echo "🗑️  Deleting block database..."
rm -rf "$BLOCKS_DB"

if [[ -d "$TXINDEX_DB" ]]; then
    echo "🗑️  Deleting transaction index..."
    rm -rf "$TXINDEX_DB"
fi

echo "✅ Chain data removed."

# ── optionally restart ────────────────────────────────────────────────────────
if [[ "$DAEMON_WAS_RUNNING" == true && -n "$TIMED_BIN" ]]; then
    echo ""
    if [[ "$SKIP_CONFIRM" == false ]]; then
        read -r -p "Restart timed daemon now? [Y/n]: " RESTART
        RESTART="${RESTART:-Y}"
    else
        RESTART="Y"
    fi

    if [[ "$RESTART" =~ ^[Yy]$ ]]; then
        EXTRA_FLAGS=""
        [[ "$NETWORK" == "testnet" ]] && EXTRA_FLAGS="--testnet"
        echo "🚀 Starting timed ${EXTRA_FLAGS}..."
        nohup "$TIMED_BIN" $EXTRA_FLAGS &>/dev/null &
        echo "   PID $! — node will sync from peers automatically."
    fi
fi

echo ""
echo "🔄 Genesis block will be recreated automatically on first start."
echo "   Monitor progress: time-cli --rpcport ${RPC_PORT} getblockchaininfo"
