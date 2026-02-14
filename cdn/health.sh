#!/usr/bin/env bash
# Health check script for MoQ CDN relay nodes.
# Fetches the BBB demo catalog from each node individually.
#
# Usage:
#   ./health.sh [webhook_url]
#
# Reads the JWT from secrets/demo-sub.jwt (same place as other tokens).
# Exit code 0 if all nodes are healthy, 1 if any failed.

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
JWT_FILE="${SCRIPT_DIR}/secrets/demo-sub.jwt"

if [ ! -f "$JWT_FILE" ]; then
	echo "Error: $JWT_FILE not found."
	echo "Generate it with: cargo run --bin moq-token -- --key secrets/root.jwk sign --root \"demo\" --get \"\" > secrets/demo-sub.jwt"
	exit 1
fi

JWT=$(cat "$JWT_FILE")
WEBHOOK_URL="${1:-}"

DOMAIN=$(cd "$SCRIPT_DIR" && tofu output -raw domain 2>/dev/null || echo "cdn.moq.dev")
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
