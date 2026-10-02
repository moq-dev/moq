# [S] An abandoned IETF group FETCH is cancelled

## Goal

When the last reader leaves a group that the Rust moq-transport subscriber is
fetching upstream, it cancels that FETCH (STOP_SENDING plus a reset of the
request stream, or FETCH_CANCEL on drafts that define it), before and after
FETCH_OK, the same as an idle SUBSCRIBE and the lite FETCH after #4691. The
partial group aborts with `Error::Cancel` and never ends clean.

## Plan

Facts (`origin/main`, 2026-10-01): `run_group_fetch` in
`rs/moq-net/src/ietf/subscriber.rs` sends the FETCH and waits only for the
request stream to close or the group slot to finish. It never watches demand.
It is cancelled only when its whole subscription ends. The subscribe path
already polls `poll_unused` and calls `cancel_subscribe`. Rust has no
FETCH_CANCEL encoding. JS never issues an IETF FETCH (`fetchGroup` refuses on
moq-transport), so it is out of scope.

Decided (2026-10-01): cancel after the answer too, matching SUBSCRIBE; the
fetched group is not filled into the cache for absent readers. Reuse
`group::Request::poll_unused` from #4691, or `demand()` if
[Demand everywhere](/quest/m1/demand-everywhere.md) lands first. Check which
drafts the tree negotiates still define FETCH_CANCEL, and which cancel by
stream reset.

Tests: a mock-time test abandons a group FETCH before and after FETCH_OK and
asserts the upstream stream is cancelled.

Public API: none. Wire: none.
