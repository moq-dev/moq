#!/usr/bin/env bash
set -euo pipefail

# Alert when system-wide available memory drops below a threshold.
# Tracks state transitions (ok <-> alerting) and uses exponential backoff.
#
# Environment variables:
#   MEMORY_ALERT_WEBHOOK    - Discord/Slack webhook URL
#   MEMORY_ALERT_THRESHOLD  - Alert when available memory drops below this % (default: 20)

WEBHOOK="${MEMORY_ALERT_WEBHOOK:-}"
THRESHOLD="${MEMORY_ALERT_THRESHOLD:-20}"
HOSTNAME=$(hostname)

STATE="ok"
BACKOFF=60 # seconds, doubles each repeated alert

send_webhook() {
	local message="$1"
	if [[ -z "$WEBHOOK" ]]; then
		return
	fi

	local payload
	payload=$(jq -n --arg content "$message" '{content: $content}')
	curl -sf -X POST -H "Content-Type: application/json" -d "$payload" "$WEBHOOK" >/dev/null || echo "Warning: webhook failed" >&2
}

top_processes() {
	ps aux --sort=-%mem | head -6
}

get_available_pct() {
	local total available
	total=$(awk '/^MemTotal:/ {print $2}' /proc/meminfo)
	available=$(awk '/^MemAvailable:/ {print $2}' /proc/meminfo)
	echo $(( available * 100 / total ))
}

while true; do
	AVAILABLE=$(get_available_pct)

	if (( AVAILABLE < THRESHOLD )); then
		if [[ "$STATE" == "ok" ]]; then
			# Transition: ok -> alerting
			STATE="alerting"
			BACKOFF=60

			MSG="Memory alert on ${HOSTNAME}: ${AVAILABLE}% available (threshold: ${THRESHOLD}%)
$(top_processes)"

			echo "$MSG"
			send_webhook "$MSG"
		else
			# Still alerting: re-post with backoff
			MSG="Memory alert on ${HOSTNAME}: ${AVAILABLE}% available (threshold: ${THRESHOLD}%)
$(top_processes)"

			echo "$MSG"
			send_webhook "$MSG"

			# Exponential backoff (uncapped)
			BACKOFF=$(( BACKOFF * 2 ))
		fi

		sleep "$BACKOFF"
	else
		if [[ "$STATE" == "alerting" ]]; then
			# Transition: alerting -> ok
			STATE="ok"

			MSG="Memory recovered on ${HOSTNAME}: ${AVAILABLE}% available"
			echo "$MSG"
			send_webhook "$MSG"
		else
			echo "OK: ${AVAILABLE}% available"
		fi

		sleep 60
	fi
done
