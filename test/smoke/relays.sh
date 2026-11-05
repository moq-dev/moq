#!/usr/bin/env bash
# Publish/subscribe through the PUBLIC MoQ relays other implementations run.
#
# The relay list is the moq-interop-runner registry (every roles.relay.remote
# endpoint), fetched live so a newly registered relay shows up without a change
# here. smoke.sh runs its usual publisher x subscriber matrix through each
# endpoint (--relay), using the same published clients as the local matrix.
#
# Third-party relays negotiate an IETF moq-transport draft rather than moq-lite,
# go down, and change underneath us, so most are OPTIONAL: they're reported in the
# summary but never fail the run. Only --required implementations gate the exit
# code (default: moq-dev's own public relay).
#
# WebSocket (qmux) is a moq-dev binding. WEBSOCKET_KEYS opts those relays in: each
# of their http(s) endpoints also runs as ws(s), its own column. Every other relay
# is dialed only on the transports it registered.
set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

REGISTRY="${RELAYS_REGISTRY:-https://raw.githubusercontent.com/englishm/moq-interop-runner/main/implementations.json}"
REQUIRED="moq-dev-rs"
REQUIRED_EXPLICIT=0 # set by --required; the default only gates when --only selects it
# Relays that speak moq-dev's WebSocket binding. Each of their http(s) endpoints
# gains a ws(s) twin. Add a key here when that implementation actually serves it.
WEBSOCKET_KEYS="moq-dev-rs,stitcher-moq"
ONLY=""
JSON_OUT=""
LOGS_OUT=""
SMOKE_ARGS=()

usage() {
    cat >&2 <<'USAGE'
usage: relays.sh [--required KEYS] [--only KEYS] [--registry URL|FILE] [--json FILE] [--logs DIR] [smoke.sh flags...]

  --required KEYS   comma-separated registry keys whose relays must pass (default: moq-dev-rs)
  --only KEYS       only test these registry keys (e.g. moq-rs-draft-18,moxygen)
  --registry SRC    implementations.json URL or path (default: moq-interop-runner main)
  --json FILE       also write the results as JSON (what report/index.html renders)
  --logs DIR        keep every cell's subscriber + publisher log; the JSON links them

Any other flag (--publishers, --subscribers, --timeout) is passed to smoke.sh.
USAGE
    exit 2
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --required | --only | --registry | --json | --logs)
            [[ $# -ge 2 && "$2" != -* ]] || usage
            case "$1" in
                --required)
                    REQUIRED="$2"
                    REQUIRED_EXPLICIT=1
                    ;;
                --only) ONLY="$2" ;;
                --registry) REGISTRY="$2" ;;
                --json) JSON_OUT="$2" ;;
                --logs) LOGS_OUT="$2" ;;
            esac
            shift 2
            ;;
        -h | --help) usage ;;
        *)
            SMOKE_ARGS+=("$1")
            shift
            ;;
    esac
done

command -v jq >/dev/null 2>&1 || {
    echo "error: jq is required" >&2
    exit 1
}

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

if [[ -f "$REGISTRY" ]]; then
    cp "$REGISTRY" "$TMP/registry.json"
elif ! curl -sfL --retry 3 --retry-all-errors -o "$TMP/registry.json" "$REGISTRY"; then
    echo "error: failed to fetch relay registry: $REGISTRY" >&2
    exit 1
fi

in_list() {
    # in_list <item> <comma-list>
    [[ ",$2," == *",$1,"* ]]
}

# websocket_url <url>: the WebSocket twin of an http(s) endpoint, or nothing.
websocket_url() {
    case "$1" in
        https://*) printf 'wss://%s\n' "${1#https://}" ;;
        http://*) printf 'ws://%s\n' "${1#http://}" ;;
        *) return 1 ;;
    esac
}

# publish_path <key>: the path a relay permits anonymous publishing under, for
# a relay that registers path-less endpoints but refuses anonymous root
# broadcasts. The URL path roots every smoke broadcast below it.
publish_path() {
    case "$1" in
        stitcher-moq) echo /moq-test ;;
        *) return 1 ;;
    esac
}

seen_url() {
    # seen_url <url>: true when selected.tsv already has this endpoint.
    awk -F '\t' -v url="$1" '$3 == url { found = 1 } END { exit !found }' "$TMP/selected.tsv"
}

# One row per endpoint: key, display name, url. Deduplicated by URL (an entry
# can list the same URL twice under different transport labels); the transport
# is taken from the URL scheme rather than the registry's label.
# The registry is third-party data: fall back to the key for a missing name,
# and drop entries without a usable URL.
jq -r '.implementations | to_entries[] | .key as $k |
    (.value.name | if type == "string" and . != "" then . else $k end) as $n |
    (.value.roles.relay.remote // [])[] |
    select((.url | type) == "string" and (.url | test("^[a-z][a-z0-9+.-]*://"))) |
    [$k, $n, .url] | @tsv' "$TMP/registry.json" |
    awk -F'\t' '!seen[$3]++' >"$TMP/endpoints.tsv"

relay_args=()
: >"$TMP/selected.tsv"
while IFS=$'\t' read -r key name url; do
    [[ -z "$ONLY" ]] || in_list "$key" "$ONLY" || continue
    if [[ "$url" =~ ^[a-z][a-z0-9+.-]*://[^/]+/?$ ]] && path=$(publish_path "$key"); then
        url="${url%/}$path"
    fi
    if seen_url "$url"; then
        continue
    fi
    relay_args+=(--relay "$url")
    printf '%s\t%s\t%s\n' "$key" "$name" "$url" >>"$TMP/selected.tsv"
    # The WebSocket column. smoke.sh dials a ws(s) URL as WebSocket and keeps the
    # fallback off for every other external URL, so this is the only attempt.
    if in_list "$key" "$WEBSOCKET_KEYS" && ws=$(websocket_url "$url") && ! seen_url "$ws"; then
        relay_args+=(--relay "$ws")
        printf '%s\t%s\t%s\n' "$key" "$name" "$ws" >>"$TMP/selected.tsv"
    fi
done <"$TMP/endpoints.tsv"

# A required implementation must not pass just because none of its endpoints
# ran: it has to exist, and an explicit --required key has to survive --only.
# (The default moq-dev-rs gate simply doesn't apply to an --only run without it.)
IFS=',' read -r -a required_keys <<<"$REQUIRED"
for key in ${required_keys[@]+"${required_keys[@]}"}; do
    if ! cut -f1 "$TMP/endpoints.tsv" | grep -qxF "$key"; then
        echo "error: required relay '$key' has no public endpoint in $REGISTRY" >&2
        exit 1
    fi
    if [[ "$REQUIRED_EXPLICIT" -eq 1 ]] && ! cut -f1 "$TMP/selected.tsv" | grep -qxF "$key"; then
        echo "error: required relay '$key' is excluded by --only $ONLY" >&2
        exit 1
    fi
done

if [[ ${#relay_args[@]} -eq 0 ]]; then
    echo "error: no relay endpoints selected from $REGISTRY" >&2
    exit 1
fi

# smoke.sh's exit status covers optional relays too; the verdict comes from the
# per-cell results instead.
: >"$TMP/results.tsv"
[[ -z "$LOGS_OUT" ]] || relay_args+=(--logs "$LOGS_OUT")
"$SMOKE_DIR/smoke.sh" "${relay_args[@]}" --results "$TMP/results.tsv" ${SMOKE_ARGS[@]+"${SMOKE_ARGS[@]}"} || true

if [[ ! -s "$TMP/results.tsv" ]]; then
    echo "error: smoke.sh recorded no results (see its output above)" >&2
    exit 1
fi

# ── summary ─────────────────────────────────────────────────────────────────
# results.json is the source of truth: one entry per endpoint (registry order),
# one cell per publisher -> subscriber pair with its status, the wire version
# each side negotiated, why it failed, and its logs (paths under --logs). The markdown table below and report/index.html both
# render it. An endpoint passes when its matrix completed (smoke.sh's DONE
# marker), at least one cell passed, and none failed; so one cut short or never
# reached (smoke.sh died mid-run) counts as failing.
run_url=""
if [[ -n "${GITHUB_RUN_ID:-}" ]]; then
    run_url="${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"
fi
jq -n --rawfile selected "$TMP/selected.tsv" --rawfile results "$TMP/results.tsv" \
    --arg required "$REQUIRED" --arg registry "$REGISTRY" --arg run_url "$run_url" \
    --arg generated "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '
    def rows(s): s | split("\n") | map(select(length > 0) | split("\t"));
    def pair: .[1] + " → " + .[2];
    rows($results) as $rows | [$rows[] | select(length > 2)] as $cells |
    [$rows[] | select(.[1] == "DONE") | .[0]] as $done | ($required | split(",")) as $req |
    {
        generated: $generated,
        run_url: (if $run_url == "" then null else $run_url end),
        registry: $registry,
        pairs: (reduce ($cells[] | pair) as $p ([]; if any(.[]; . == $p) then . else . + [$p] end)),
        endpoints: [rows($selected)[] as [$key, $name, $url] |
            [$cells[] | select(.[0] == $url)] as $mine | {
                key: $key,
                name: $name,
                url: $url,
                transport: (if $url | startswith("moqt:") then "QUIC"
                    elif $url | test("^wss?:") then "WebSocket"
                    else "WebTransport" end),
                required: any($req[]; . == $key),
                ok: (any($done[]; . == $url) and any($mine[]; .[3] == "PASS") and all($mine[]; .[3] != "FAIL")),
                versions: ([$mine[] | .[4], .[5]] | map(select(. != null and . != "")) | unique),
                cells: ($mine | map({key: pair, value: {
                    status: .[3],
                    pub_version: (.[4] // ""),
                    sub_version: (.[5] // ""),
                    reason: (.[6] // ""),
                    sub_log: (.[7] // ""),
                    pub_log: (.[8] // "")
                }}) | from_entries)
            }]
    }' >"$TMP/results.json"
[[ -n "$JSON_OUT" ]] && cp "$TMP/results.json" "$JSON_OUT"

jq -r '
    def icon: if . == "PASS" then "✅" elif . == "FAIL" then "❌" elif . == "SKIP" then "➖" else " " end;
    .pairs as $pairs |
    [.endpoints[] | select(.required)] as $req | [.endpoints[] | select(.required | not)] as $opt |
    "## External relay interop\n",
    "| Relay | Transport | Endpoint | Gate | Version | " + ($pairs | join(" | ")) + " |",
    "|---|---|---|---|---|" + ($pairs | map("---|") | join("")),
    (.endpoints | sort_by([(.name | ascii_downcase), .key, .transport]) | .[] | . as $e |
        "| \(.name) (`\(.key)`) | \(.transport) | `\(.url)` | \(if .required then "required" else "optional" end) | \(.versions | join(", ")) | "
        + ($pairs | map($e.cells[.].status // "" | icon) | join(" | ")) + " |"),
    "\nRequired: \([$req[] | select(.ok)] | length)/\($req | length) endpoints passing. Optional: \([$opt[] | select(.ok)] | length)/\($opt | length) endpoints passing.",
    "✅ bytes flowed · ❌ no data / error · ➖ skipped (client can'"'"'t dial that transport)"
' "$TMP/results.json" >"$TMP/summary.md"

echo
cat "$TMP/summary.md"
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    cat "$TMP/summary.md" >>"$GITHUB_STEP_SUMMARY"
fi

failed_required=$(jq -r '[.endpoints[] | select(.required and (.ok | not)) | .url] | join(" ")' "$TMP/results.json")
if [[ -n "$failed_required" ]]; then
    echo "relays: required relay(s) failed: $failed_required" >&2
    exit 1
fi
echo "relays: all required relays passed"
