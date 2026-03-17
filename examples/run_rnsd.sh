#!/usr/bin/env bash
#
# Start the system rnsd daemon with the config next to this script.
#
# Prerequisites:
#   pip install rns
#
# Usage:
#   ./examples/run_rnsd.sh          # default verbosity
#   ./examples/run_rnsd.sh -v       # verbose
#   ./examples/run_rnsd.sh -vvvvvv  # extreme logging

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CONFIG_DIR="$SCRIPT_DIR/rnsd_config"

if ! command -v rnsd &>/dev/null; then
    echo "Error: rnsd not found. Install it with: pip install rns" >&2
    exit 1
fi

echo "Starting rnsd with config: $CONFIG_DIR"
exec rnsd --config "$CONFIG_DIR" "$@"
