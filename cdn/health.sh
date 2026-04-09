#!/usr/bin/env bash
# Health check script for MoQ CDN relay nodes.
# Fetches the BBB demo catalog from each node individually (anonymous subscribe).
#
# Usage:
#   ./health.sh [webhook_url]
#
# Exit code 0 if all nodes are healthy, 1 if any failed.

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
WEBHOOK_URL="${1:-}"

DOMAIN=$(cd "$SCRIPT_DIR/common" && tofu output -raw domain 2>/dev/null) || {
	echo "Warning: could not read domain from tofu state, falling back to cdn.moq.dev" >&2
	DOMAIN="cdn.moq.dev"
}
NODES=("usc" "usw" "use" "euc" "sea")

failed=()

for node in "${NODES[@]}"; do
	url="https://${node}.${DOMAIN}/fetch/demo/bbb/catalog.json"
	printf "%-4s %s.%s ... " "[$node]" "$node" "$DOMAIN"

	status=$(curl -sf -o /dev/null -w "%{http_code}" --max-time 10 "$url" 2>/dev/null) && ok=true || ok=false

	if $ok && [ "$status" = "200" ]; then
		echo "OK (${status})"
	else
		echo "FAIL (${status:-timeout})"
		failed+=("$node")
	fi
done

echo ""

if [ ${#failed[@]} -eq 0 ]; then
	echo "All nodes healthy."
	exit 0
fi

msg="MoQ CDN health check FAILED for: ${failed[*]}"
echo "$msg"

# Post to a webhook (Slack, Discord, etc.) if provided
if [ -n "$WEBHOOK_URL" ]; then
	payload=$(jq -n --arg content "$msg" '{content: $content}')
	curl -sf -X POST -H "Content-Type: application/json" \
		-d "$payload" \
		"$WEBHOOK_URL" >/dev/null 2>&1 || echo "Warning: webhook post failed"
fi

exit 1
