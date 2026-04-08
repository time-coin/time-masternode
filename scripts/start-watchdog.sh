#!/usr/bin/env bash
# Launches mn-watchdog.sh inside a named screen session called "watchdog".

SESSION="watchdog"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_DIR" || { echo "ERROR: cannot cd into $PROJECT_DIR"; exit 1; }

if screen -list | grep -q "\.${SESSION}"; then
  echo "Screen session '${SESSION}' is already running — attaching..."
  exec screen -r "$SESSION"
fi

# Start watchdog directly in a new screen session (attached).
# Detach with Ctrl+A D; reattach later with: screen -r watchdog
exec screen -S "$SESSION" bash "$SCRIPT_DIR/mn-watchdog.sh" "$@"
