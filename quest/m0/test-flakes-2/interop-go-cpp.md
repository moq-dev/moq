# [S] Interop's go -> cpp cell holds up under load

## Goal

`just test interop --all` passes the `go -> cpp` media cell, the
`rust-swapped -> js` and `rust-swapped -> js-native-node` wrong-tone cells,
and the Python/Rust sockets lane on a loaded host, and their quarantines in
`test/interop/interop.sh` (`run_round`, and the `run_sockets` call) are
removed.

## Plan

Planned 2026-11-10 under the repository's flaky-test rule, while iterating
[moq-dev/moq#8457](https://github.com/moq-dev/moq/pull/8457). One local
`just test interop --all` failed `go -> cpp` after 18 of 30 seconds and
passed on rerun; every other `go ->` cell and `cpp -> go` passed. The cell
runs on `moq-lite-07-wip`: the auto pass pins the Go and C++ clients to
`AUTH_VERSION`, so that PR's `Serves` on SUBSCRIBE is in its path. Both ends
were built from that PR, and a codec mismatch fails a session's first
SUBSCRIBE rather than 18 seconds in, so the wire change is an unlikely cause
but not ruled out; the failing run's logs are gone.

The cpp subscriber's log, after its grant of `subscribe=["**/<broadcast>"]`
and `publish=[]`, ends with `moq_net::lite::publisher` logging
`publishing outside our grant; closing the session` for the broadcast it
was subscribing to, then `container consumer: unroutable`. A subscribe-only
client found the broadcast it consumes among its own publications: suspect
the cpp client (or `moq-ffi` beneath it) sharing one origin between what it
publishes and what it consumes, so a consumed broadcast is sometimes
announced back to the relay. Reproduce with
`INTEROP_QUARANTINED=1 just test interop --all --publishers go --subscribers cpp`
in a loop under load, find why the consumed broadcast reaches the publish side, and fix it
there.

A later full run on the same branch, with this cell quarantined, failed
three other lanes once each, all passing when rerun, so they are quarantined
here too (maintainer, 2026-11-11): `rust-swapped -> js` timed out after
decoding only the opus rendition into the swapped tone, and
`rust-swapped -> js-native-node` ended with `Error: unroutable`, both with
a `moq-lite-07-wip` Rust publisher and a JS subscriber that logged
`moq-lite-06`; the Python/Rust sockets lane on `moq-lite-07-wip` also ended
with only `Error: unroutable`, and passed in three narrowed reruns. Check
whether they share this cell's cause, and that of
[interop-python-go.md](/quest/m0/test-flakes-2/interop-python-go.md), whose
Go subscriber also closed its session for publishing outside its grant.
`INTEROP_QUARANTINED=1` runs every quarantined lane, the sockets lane
included.
