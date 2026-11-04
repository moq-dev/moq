#!/usr/bin/env bash
# Run the harness in one browser. Usage: run.sh <name> <port> <runs> -- <browser command with URL as {url}>
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
name=$1; port=$2; runs=$3; shift 4
rm -f "$here/results/$name.json"
python3 -I "$here/scripts/serve.py" "$here/www" "$here/results" "$port" &
server=$!
trap 'kill $server 2>/dev/null || true' EXIT
sleep 1
url="http://127.0.0.1:$port/index.html?browser=$name&runs=$runs"
cmd=(); for a in "$@"; do if [ "$a" = "{url}" ]; then cmd+=("$url"); else cmd+=("$a"); fi; done
"${cmd[@]}" >"$here/results/$name.browser.log" 2>&1 &
browser=$!
for _ in $(seq 1 600); do
	[ -f "$here/results/$name.json" ] && break
	if ! kill -0 $browser 2>/dev/null && [ "${DETACHED:-}" != 1 ]; then echo "browser exited early"; exit 1; fi
	sleep 1
done
kill $browser 2>/dev/null || true
[ -f "$here/results/$name.json" ] || { echo "timed out"; exit 1; }
echo "ok $name"
