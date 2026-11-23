#!/usr/bin/env bash
# Runs the calmcp MCP server against the local Radicale instance
# (dev/radicale). Any variable can be overridden from the environment.
set -euo pipefail

cd "$(dirname "$0")/.."

export CALDAV_URL="${CALDAV_URL:-http://localhost:5232/}"
export CALDAV_USERNAME="${CALDAV_USERNAME:-test}"
export CALDAV_PASSWORD="${CALDAV_PASSWORD:-test}"
export MCP_API_KEY="${MCP_API_KEY:-dev}"
export PORT="${PORT:-8000}"
export TZ="${TZ:-Etc/UTC}"
export RUST_LOG="${RUST_LOG:-info}"

if ! curl -s -o /dev/null "$CALDAV_URL"; then
    echo "CalDAV server not reachable at $CALDAV_URL" >&2
    echo "start it with: docker compose -f dev/radicale/docker-compose.yml up -d" >&2
    exit 1
fi

echo "MCP endpoint: http://localhost:$PORT/mcp (Authorization: Bearer $MCP_API_KEY)"
exec cargo run -- "$@"
