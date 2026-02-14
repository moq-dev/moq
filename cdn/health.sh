#!/usr/bin/env bash
# Health check script for MoQ CDN relay nodes.
# Fetches the BBB demo catalog from each node individually.
#
# Usage:
#   ./health.sh <jwt>
#   ./health.sh <jwt> [webhook_url]
#
# Exit code 0 if all nodes are healthy, 1 if any failed.

set -euo pipefail

JWT="${1:?Usage: $0 <jwt> [webhook_url]}"
WEBHOOK_URL="${2:-}"

DOMAIN=$(cd "$(dirname "$0")" && tofu output -raw domain 2>/dev/null || echo "cdn.moq.dev")
NODES=("usc" "euc" "sea")
PATH_AND_QUERY="/fetch/demo/bbb/catalog.json?jwt=${JWT}"

failed=()

for node in "${NODES[@]}"; do
	url="https://${node}.${DOMAIN}${PATH_AND_QUERY}"
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
	curl -sf -X POST -H "Content-Type: application/json" \
		-d "{\"text\": \"$msg\"}" \
		"$WEBHOOK_URL" >/dev/null 2>&1 || echo "Warning: webhook post failed"
fi

exit 1
