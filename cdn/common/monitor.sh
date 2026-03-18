#!/usr/bin/env bash
set -euo pipefail

# Monitor script for MoQ CDN nodes.
# Runs memory and health checks in a loop, posting to a Discord webhook.
#
# Environment variables:
#   MONITOR_WEBHOOK           - Discord webhook URL
#   MONITOR_MEMORY_THRESHOLD  - Alert when available memory drops below this % (default: 20)
#   MONITOR_HEALTH_DOMAIN     - Domain name for health checks (default: cdn.moq.dev)
#   MONITOR_HEALTH_JWT        - Path to subscriber JWT (default: /var/lib/moq/demo-sub.jwt)
#   MONITOR_HEALTH_NODES      - Space-separated list of nodes (default: "usc euc sea")

WEBHOOK="${MONITOR_WEBHOOK:-}"
MEMORY_THRESHOLD="${MONITOR_MEMORY_THRESHOLD:-20}"
HEALTH_DOMAIN="${MONITOR_HEALTH_DOMAIN:-cdn.moq.dev}"
HEALTH_JWT_FILE="${MONITOR_HEALTH_JWT:-/var/lib/moq/demo-sub.jwt}"
IFS=' ' read -r -a HEALTH_NODES <<< "${MONITOR_HEALTH_NODES:-usc euc sea}"
HOSTNAME=$(hostname)

MEMORY_STATE="ok"
MEMORY_BACKOFF=60
MAX_MEMORY_BACKOFF=600

# Track per-node health state for transition alerts
declare -A HEALTH_STATE
for node in "${HEALTH_NODES[@]}"; do
	HEALTH_STATE[$node]="ok"
done

send_webhook() {
	local message="$1"
	if [[ -z "$WEBHOOK" ]]; then
		return
	fi

	local payload
	payload=$(jq -n --arg content "$message" '{content: $content}')
	curl -sf --connect-timeout 5 --max-time 10 -X POST -H "Content-Type: application/json" -d "$payload" "$WEBHOOK" >/dev/null || echo "Warning: webhook failed" >&2
}

# ── Memory check ────────────────────────────────────────────────────

get_available_pct() {
	local total available
	total=$(awk '/^MemTotal:/ {print $2}' /proc/meminfo)
	available=$(awk '/^MemAvailable:/ {print $2}' /proc/meminfo)
	echo $(( available * 100 / total ))
}

check_memory() {
	local available
	available=$(get_available_pct)

	if (( available < MEMORY_THRESHOLD )); then
		local top
		top=$(ps -eo pid,user,%mem,rss,comm --sort=-%mem | head -6)

		local msg="Memory alert on ${HOSTNAME}: ${available}% available (threshold: ${MEMORY_THRESHOLD}%)
${top}"
		echo "$msg"
		send_webhook "$msg"

		if [[ "$MEMORY_STATE" == "ok" ]]; then
			MEMORY_STATE="alerting"
			MEMORY_BACKOFF=60
		else
			MEMORY_BACKOFF=$(( MEMORY_BACKOFF * 2 ))
			if (( MEMORY_BACKOFF > MAX_MEMORY_BACKOFF )); then
				MEMORY_BACKOFF=$MAX_MEMORY_BACKOFF
			fi
		fi
	else
		if [[ "$MEMORY_STATE" == "alerting" ]]; then
			MEMORY_STATE="ok"
			MEMORY_BACKOFF=60
			local msg="Memory recovered on ${HOSTNAME}: ${available}% available"
			echo "$msg"
			send_webhook "$msg"
		fi
	fi
}

# ── Health check ────────────────────────────────────────────────────

check_health() {
	if [ ! -f "$HEALTH_JWT_FILE" ]; then
		echo "Health check skipped: $HEALTH_JWT_FILE not found"
		return
	fi

	local jwt
	jwt=$(cat "$HEALTH_JWT_FILE")

	for node in "${HEALTH_NODES[@]}"; do
		local url="https://${node}.${HEALTH_DOMAIN}/fetch/demo/bbb/catalog.json?jwt=${jwt}"
		local status ok=false

		status=$(curl -sf -o /dev/null -w "%{http_code}" --max-time 10 "$url" 2>/dev/null) && ok=true

		if $ok && [ "$status" = "200" ]; then
			if [[ "${HEALTH_STATE[$node]}" == "failing" ]]; then
				HEALTH_STATE[$node]="ok"
				local msg="Health recovered: ${node}.${HEALTH_DOMAIN}"
				echo "$msg"
				send_webhook "$msg"
			fi
		else
			if [[ "${HEALTH_STATE[$node]}" != "failing" ]]; then
				HEALTH_STATE[$node]="failing"
				local msg="Health check FAILED: ${node}.${HEALTH_DOMAIN} (${status:-timeout})"
				echo "$msg"
				send_webhook "$msg"
			else
				echo "[$node] still failing (${status:-timeout})"
			fi
		fi
	done
}

# ── Main loop ───────────────────────────────────────────────────────

while true; do
	check_memory
	check_health

	# Use memory backoff if alerting, otherwise check every 60s
	if [[ "$MEMORY_STATE" == "alerting" ]]; then
		sleep "$MEMORY_BACKOFF"
	else
		sleep 60
	fi
done
