# [S] A relay answers a SUBSCRIBE on a copy it kept after a fetch

## Goal

A moq-transport subscriber gets SUBSCRIBE_OK for a history SUBSCRIBE sent
through a relay that holds a copy kept after a FETCH. The OK names the live
edge from the upstream's SUBSCRIBE_OK, never from the relay's cache or an
edge the upstream reported earlier. An integration test covers it.

## Plan

Planned 2026-11-01 (maintainer, follow-up of
[moq-dev/moq#7545](https://github.com/moq-dev/moq/pull/7545)).

Repro, from #7545's review: the relay fetches group 2 of an unfinished track.
A fresh draft-22 session then subscribes `with_range(1..2)` through it, and
gets no SUBSCRIBE_OK within 5 s. It hangs the same way on main
(`3ae67897`), where the relay does subscribe upstream, so this bug predates
#7545.

- The maintainer decided on 2026-11-01, on #7545, that the OK waits until the
  live edge is known, and is never taken from the cache. So the fix is not to
  answer from the stale reported edge.
- Find why the upstream's SUBSCRIBE_OK doesn't reach the downstream OK on a
  fetched copy, and fix that.
- Test it in `rs/moq-net/tests/integration/fetch_only.rs` on drafts 18 and 22,
  direct and through a relay.

Checkpoint 2026-11-02: claimed in
[moq-dev/moq#7631](https://github.com/moq-dev/moq/pull/7631), but validation
stops before compilation. `quest gate nix develop --command just rs test
-p moq-net -E 'test(a_subscribe_after_track_status_keeps_timestamps)'`
fails when mbx launches Cargo: `Argument list too long (os error 7)`.
The existing repo-side launcher fix is
[moq-dev/moq#7594](https://github.com/moq-dev/moq/pull/7594). No validation
bypass or source change was made; reproduction, implementation, and the
four-case matrix remain undone. Resume this claim once standard validation
works.

Source inspection finds two waits to investigate: the linger handler's
`wants_feed` excludes wholly historical demand, while `subscribe_track`
waits for `poll_live`; and resuming a copy waits for its fill head before
`set_live`. Distinguish a fresh upstream answer from cache readiness without
answering from an earlier report. These are unverified hypotheses, not an
executed reproduction. Inspected the open range/origin work, including
[moq-dev/moq#7625](https://github.com/moq-dev/moq/pull/7625) and
[moq-dev/moq#7624](https://github.com/moq-dev/moq/pull/7624); their scopes and
ownership remain unchanged.

Public API: none. Wire: none.
