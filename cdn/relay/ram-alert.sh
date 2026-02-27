#!/usr/bin/env bash
# RAM usage alert for MoQ CDN relay nodes.
# Checks available memory and swap usage, alerts via webhook on threshold breach.
#
# Usage:
#   ./ram-alert.sh [webhook_url]
#
# Environment variables:
#   RAM_ALERT_WEBHOOK   - webhook URL (overridden by argument)
#   RAM_ALERT_THRESHOLD - alert when available memory drops below this % (default: 20)

set -euo pipefail

WEBHOOK_URL="${1:-${RAM_ALERT_WEBHOOK:-}}"
THRESHOLD="${RAM_ALERT_THRESHOLD:-20}"
HOSTNAME=$(hostname)

# Parse /proc/meminfo (values in kB)
mem_total=$(awk '/^MemTotal:/ {print $2}' /proc/meminfo)
mem_available=$(awk '/^MemAvailable:/ {print $2}' /proc/meminfo)
swap_total=$(awk '/^SwapTotal:/ {print $2}' /proc/meminfo)
swap_free=$(awk '/^SwapFree:/ {print $2}' /proc/meminfo)
swap_used=$((swap_total - swap_free))

# Calculate available percentage
mem_pct=$((mem_available * 100 / mem_total))

# Convert to human-readable MB
mem_total_mb=$((mem_total / 1024))
mem_available_mb=$((mem_available / 1024))
swap_used_mb=$((swap_used / 1024))

alert=false
reasons=""

if [ "$mem_pct" -lt "$THRESHOLD" ]; then
	alert=true
	reasons="${reasons}- Available memory: ${mem_available_mb}MB / ${mem_total_mb}MB (${mem_pct}%)\n"
fi

if [ "$swap_used" -gt 0 ]; then
	alert=true
	reasons="${reasons}- Swap in use: ${swap_used_mb}MB\n"
fi

if [ "$alert" = "false" ]; then
	echo "OK: ${mem_available_mb}MB available (${mem_pct}%), no swap"
	exit 0
fi

# Get top memory consumers
top_procs=$(ps aux --sort=-%mem | head -6 | tail -5 | awk '{printf "  %s (PID %s): %s%% RAM, %.0f MB RSS\n", $11, $2, $4, $6/1024}')

msg="RAM alert on ${HOSTNAME}:\n${reasons}\nTop processes by memory:\n${top_procs}"

echo -e "$msg"

# Send webhook if configured
if [ -n "$WEBHOOK_URL" ]; then
	json_payload=$(echo -e "$msg" | python3 -c 'import json,sys; print(json.dumps({"content": sys.stdin.read()}))')
	curl -sf -X POST -H "Content-Type: application/json" \
		-d "$json_payload" \
		"$WEBHOOK_URL" >/dev/null 2>&1 || echo "Warning: webhook post failed"
fi

exit 1
