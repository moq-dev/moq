# [S] Interop's go -> cpp cell holds up under load

## Goal

`just test interop --all` passes the `go -> cpp` media cell on a loaded
host, and its quarantine in `test/interop/interop.sh`'s `run_round` is
removed.

## Plan

Planned 2026-11-10 under the repository's flaky-test rule, while iterating
[moq-dev/moq#8457](https://github.com/moq-dev/moq/pull/8457). One local
`just test interop --all` failed `go -> cpp` after 18 of 30 seconds and
passed on rerun; every other `go ->` cell and `cpp -> go` passed. The matrix
negotiates its default version there, not `moq-lite-07-wip`, so that PR's
lite-07 wire changes are not in its path.

The cpp subscriber's log, after its grant of `subscribe=["**/<broadcast>"]`
and `publish=[]`, ends with `moq_net::lite::publisher` logging
`publishing outside our grant; closing the session` for the broadcast it
was subscribing to, then `container consumer: unroutable`. A subscribe-only
client found the broadcast it consumes among its own publications: suspect
the cpp client (or `moq-ffi` beneath it) sharing one origin between what it
publishes and what it consumes, so a consumed broadcast is sometimes
announced back to the relay. Reproduce with
`just test interop --all --publishers go --subscribers cpp` in a loop under
load, find why the consumed broadcast reaches the publish side, and fix it
there.

A later full run on the same branch, with this cell quarantined, failed
three other lanes once each, all passing when rerun; they are recorded
here, not quarantined: `rust-swapped -> js` timed out
after decoding only the opus rendition into the swapped tone, and
`rust-swapped -> js-native-node` ended with `Error: unroutable`, both on
`moq-lite-06`; the Python/Rust sockets lane on `moq-lite-07-wip` also ended
with only `Error: unroutable`, and passed in three narrowed reruns. Check
whether they share this cell's cause before quarantining them.
