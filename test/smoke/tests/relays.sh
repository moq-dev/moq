#!/usr/bin/env bash
# Exercise endpoint selection and required-relay gates without public services.
set -euo pipefail
unset FAIL_URL GITHUB_STEP_SUMMARY GITHUB_RUN_ID
script=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/relays.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cp "$script" "$work/relays.sh"
cat >"$work/smoke.sh" <<'SMOKE'
#!/usr/bin/env bash
set -euo pipefail
urls=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --relay) urls+=("$2"); shift ;;
        --results) results=$2; shift ;;
        *) exit 2 ;;
    esac
    shift
done
for url in "${urls[@]}"; do
    status=PASS
    [[ "$url" != "${FAIL_URL:-}" ]] || status=FAIL
    printf '%s\trust\trust\t%s\n%s\tDONE\n' "$url" "$status" "$url" >>"$results"
done
SMOKE
chmod +x "$work/smoke.sh"
cat >"$work/registry.json" <<'JSON'
{"implementations": {
  "moq-dev-rs": {"roles": {"relay": {"remote": [{"url": "https://moq.example"}]}}},
  "stitcher-moq": {"roles": {"relay": {"remote": [
    {"url": "https://stitcher.example"},
    {"url": "https://stitcher.example"},
    {"url": "http://stitcher.example/custom"},
    {"url": "moqt://stitcher.example"}
  ]}}},
  "other": {"roles": {"relay": {"remote": [{"url": "https://other.example"}]}}}
}}
JSON

bash "$work/relays.sh" --registry "$work/registry.json" --json "$work/results.json" >"$work/log" 2>&1
jq -e '[.endpoints[].url] == [
    "https://moq.example", "wss://moq.example",
    "https://stitcher.example/moq-test", "wss://stitcher.example/moq-test",
    "http://stitcher.example/custom", "ws://stitcher.example/custom",
    "moqt://stitcher.example/moq-test", "https://other.example"
]' "$work/results.json" >/dev/null
echo 'ok: WebSocket twins preserve publish paths, deduplicate URLs, and stay opt-in'

export FAIL_URL=wss://stitcher.example/moq-test
bash "$work/relays.sh" --registry "$work/registry.json" --json "$work/results.json" >"$work/log" 2>&1
jq -e '.endpoints[] | select(.url == env.FAIL_URL) | .ok == false and .required == false' "$work/results.json" >/dev/null
if bash "$work/relays.sh" --registry "$work/registry.json" --required stitcher-moq >"$work/log" 2>&1; then
    echo 'expected the required WebSocket endpoint to fail the run' >&2
    exit 1
fi
grep -qF "$FAIL_URL" "$work/log"
echo 'ok: a failed WebSocket cell is reported and fails only a required relay'
