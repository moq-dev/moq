# [S] Interop's python -> go media cell holds up

## Goal

The `python -> go` media cell of `just test interop --all` passes reliably,
and its quarantine in `test/interop/interop.sh`'s `run_round` is removed.

## Plan

Found 2026-11-10 while iterating
[moq-dev/moq#8444](https://github.com/moq-dev/moq/pull/8444), under the
flaky-test rule. One local `just test interop --all` failed only this cell
and the rerun passed it. On moq-lite-07-wip the Go subscriber (through
moq-ffi) logged `auth granted publish=[] subscribe=["**/<broadcast>"]`.
About 6 s later it logged `publishing outside our grant; closing the session`
from `moq_net::lite::publisher`, and then `audio gap after 0 samples`.

The subscriber holds a publish grant of nothing, so whatever its session
judged to be published, such as a broadcast echoed back through its origin or
a reconnect's re-announce, should never have reached the grant check. Reproduce
it with the interop logs kept, find what the Go client's origin advertises,
fix the cause, then remove the quarantine. A real bug in moq-net's grant
enforcement or in moq-ffi's origin wiring is in scope.

Attempted 2026-11-11 without reproducing it: the cell passed 6 local runs of
`--publishers python --subscribers rust,python,go,cpp,c --versions moq-lite-07-wip`
(1 to 17 s), and all 24 of its runs in the scheduled Interop logs from
2026-11-04 to 2026-11-09. The failure stalled for 6 s with no audio, while a
passing cell finishes in about 1 s, so it needs whatever stalled that run.
`auth::lite_07::a_broadcast_relayed_from_another_client_is_not_ours_to_check`
pins the three-party shape (publisher, relay, shared-origin subscriber) and passes.

The lead, unconfirmed: a lite-07 claim the subscriber learns through
`lite/subscriber/routed.rs` enters its origin with `via` set to the session's
assigned peer hop and a chain ending at the claim's origin node, while the
publisher's split horizon excludes the relay's declared id. If neither names the
relay, as after a reconnect leaves a lingering route behind or when the chain
omits the relay, the claim is visible to the grant check. That is route-layer
code, out of scope for routing-free work.

Decided 2026-11-12 by the maintainer: ✅ keep the lane quarantined and pick up
the lead above once the routing work lands (rejected: unquarantine to collect a
nightly failing log; close the quest on the clean history). The routing work is
[moq-dev/moq#8457](https://github.com/moq-dev/moq/pull/8457) and
[moq-dev/moq#8443](https://github.com/moq-dev/moq/pull/8443), which change the
same path. This quest's PR lands after both, so the quest is ready once they are in.

Public API: none expected. Wire: none.
