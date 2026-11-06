#!/usr/bin/env bash
set -euo pipefail
# shellcheck source-path=SCRIPTDIR source=relay-log.sh
source "$(dirname "${BASH_SOURCE[0]}")/relay-log.sh"
HARNESS_RUN=$(mktemp -d)
trap 'rm -rf "$HARNESS_RUN"' EXIT

# Connections from an earlier round cannot satisfy the current round's accounting.
printf '%s\n' 'INFO conn{id=1 remote=localhost}: accepted' 'INFO connection closed id=1' >"$HARNESS_RUN/relay.log"
overall=0
if check_idle_outs empty 3 >"$HARNESS_RUN/result"; then exit 1; fi
[[ "$overall" == 1 ]]
grep 'relay log contains no connections' "$HARNESS_RUN/result"

# A clean close is observed even with ANSI formatting, and an older close is ignored.
printf '\033[32mINFO\033[0m conn{id=2 remote=localhost}: accepted\nINFO connection closed id=1\nINFO connection closed id=2\n' >>"$HARNESS_RUN/relay.log"
[[ "$(relay_connections 3)" == '2 ended' ]]
overall=0
check_idle_outs clean 3
[[ "$overall" == 0 ]]

printf '%s\n' 'INFO conn{id=3 remote=localhost}: accepted' 'WARN connection closed id=3 err=timed out' >>"$HARNESS_RUN/relay.log"
overall=0
if check_idle_outs timeout 6 >"$HARNESS_RUN/result"; then exit 1; fi
[[ "$overall" == 1 ]]
grep 'idled out instead of closing' "$HARNESS_RUN/result"

# An earlier round's connection that logs or idles out in this window isn't counted.
printf '%s\n' 'WARN conn{id=1 remote=localhost}: refused' 'INFO conn{id=4 remote=localhost}: accepted' 'WARN connection closed id=3 err=timed out' 'INFO connection closed id=4' >>"$HARNESS_RUN/relay.log"
[[ "$(relay_connections 8)" == '4 ended' ]]

# A pinned round fails on any connection that negotiated another version, and only counts its own.
printf '%s\n' 'INFO conn{id=5 remote=localhost session="a"}: moq_relay::connection: negotiated version=moq-lite-06 transport=quic' \
    'INFO conn{id=6 remote=localhost session="b"}: moq_relay::connection: negotiated version=moq-transport-18 transport=webtransport' >>"$HARNESS_RUN/relay.log"
check_versions pinned 13 moq-transport-18
if check_versions pinned 12 moq-transport-18 >"$HARNESS_RUN/result"; then exit 1; fi
grep -x '  FAIL  pinned round: relay connection 5 negotiated moq-lite-06, not moq-transport-18' "$HARNESS_RUN/result"
echo 'relay log regression passed'
