# [L] A WebSocket-to-QUIC upgrade restarts epochless broadcasts explicitly

## Goal

When a client's session upgrades from the WebSocket fallback to QUIC, every
epochless broadcast it had resolved gets an explicit `Restart` announce
event, and every first-party consumer follows it by resolving the path
again. Interop's full matrix on `main` stops failing with `unroutable`,
and its idle-out failures go away with it.

Non-goals: carrying an epochless route's identity across the upgrade, and
treating `Unroutable` as a cue to retry. Epoch routes are unchanged: a
matching epoch already continues across the upgrade.

## Plan

Seen 2026-10-12 and 2026-10-13: Interop fails on the nightly schedule run of
`main` (run 38284317921) and on every PR that touches `test/interop`
(#5381's run 38337824317, among others). Only the cross-client rounds fail;
every `tail` case passes. The same cells pass on one run and fail on the next:

- Rust (`moq export fmp4`), Go, and C++ subscribers exit with `unroutable`
  (`catalog consumer: unroutable`, `container consumer: unroutable`), from
  any publisher.
- The C subscriber reports `session status: 1`, then `2`, then `timed out
  waiting for data`.
- The round then fails with `relay connection N (subscriber) idled out
  instead of closing`: a client that failed exited without closing its
  session.

Cause, from the logs. In the failing js -> rust cell, the Rust subscriber
logs `connected version=moq-lite-07-wip transport=websocket`, then
`upgraded from WebSocket ... transport=webtransport` 30 ms later, then
`track info failed ... track=catalog.json err=unroutable`. On the CI runner
the debug-build QUIC dial (it fetches the certificate fingerprint over HTTP
first) often loses the race to the WebSocket head start, so the
upgrade from #4180 (2026-10-06) runs right after connect. Since #5294
(f0fcbefd6, merged 2026-10-11 19:23 UTC), an epochless route's requests
carry a `RequestValidity` (`rs/moq-net/src/model/broadcast.rs`). It reports
`Unroutable` once the best route from the same `via` is a different
`Instance`, and an epochless instance is the route entry's generation
(`RouteEntry::instance` in `model/origin.rs`). The upgraded session
re-announces the same paths from the same peer under new generations. Every
handle resolved before the upgrade is then treated as retired, and its next
request fails. Epochless is the default: lite-06 carries no epoch, and the
browser and binding publishers don't set one.

Onset matches. The first Interop run with `unroutable` started at 20:09 UTC on
2026-10-11. The last red runs before it failed for other reasons (`media`,
`ts`). Runs before #5294 have no `unroutable`. It still passes sometimes,
whenever QUIC wins the race. Users hit it too: `moq export` on a default
connection exits whenever WebSocket wins briefly.

Reproduced while planning, on `main` at 079d7d8d4, by extending
`websocket_upgrades_to_quic` (`rs/moq-tokio/src/connection.rs`) with an
epochless `audio` track that nothing subscribes until after the upgrade:

- A subscribe through the pre-upgrade `cam` handle ends with `Unroutable`.
  With `RequestValidity::check` forced to pass, it succeeds.
- An announce consumer opened on the subscriber origin before the upgrade
  does get `Restart` for `cam`, but only after the origin's `update_hold`
  (300 ms by default; `TableCursor::update` holds epochless restarts, and
  `identical_reannounce_restarts_without_an_epoch` covers the event). A
  first check within about 300 ms saw nothing; a 2 s wait saw the `Restart`.
  Codex caught this on #5388's review.
- A fresh `routed_broadcast("cam")` after the upgrade subscribes `audio`
  fine.

So the signal exists, but it arrives late: `RequestValidity` retires the old
handle as soon as the new route wins, while its `Restart` waits out the hold.
A consumer that asks for a track inside that window, which every interop
subscriber does right after connecting, gets `Unroutable` before any
`Restart` could reach it. And no interop consumer follows `Restart` anyway.

Decided by the maintainer 2026-10-13 on #5388: an upgrade on an epochless
route is a Restart. "No re-requests after Unroutable please, explicitly
Restart only." This matches the
[transport upgrade](/quest/m1/transport-upgrade/README.md) plan ("epochless
routes instead signal Restart and need a fresh follower resolution"). The
other options, carrying the route identity across the upgrade and not
upgrading epochless sessions, were rejected.

1. Retirement and its Restart agree. A holder must never see `Unroutable`
   from a retired handle before the `Restart` it could have followed has been
   delivered. The origin already emits the `Restart`; align the two in the
   origin, not in moq-tokio, so a relay hop behaves the same when a publisher
   upgrades. The relay's lite subscriber builds the same validity for a
   publisher's session; check that hop too. Recommended: a handle's
   `RequestValidity` retires only once its held `Restart` becomes
   deliverable, so the old route keeps serving new requests through the
   hold. Queueing is not enough: today `OriginState::sync_route` queues the
   `PendingUpdate::Restart` (via `TableCursor::update`) before
   `routes.poke_below` invalidates the `Selection`, and the hold is only
   applied later in `AnnounceConsumer::poll_next`. The old session is
   still draining, so it can. The alternative is to skip the hold for a
   replacement that retires handles, but the hold exists to absorb path
   hunting after a withdrawal, so check what that would cost first. Don't
   change same-`via` handling: an identical epochless re-announcement already
   restarts. Subscriptions already open stay sticky on the old session until
   it drains, as today.
2. First-party consumers follow it. Each holds the announce stream for the
   path it resolved, and on `Restart` drops its handles and resolves afresh.
   On `Unroutable` it fails; it never retries a request. Audit and fix:
   - Rust: `moq export` and `moq subscribe` (the interop subscriber), plus
     any other moq-cli command that resolves a broadcast and keeps reading.
     `moq play` already follows `Restart` (see
     [Broadcast epochs](/quest/m0/broadcast-epoch/README.md)); confirm it.
   - Bindings: moq-ffi's catalog and container consumers, as Go, C++, and
     Python reach them, and moq-c's (`moq_origin_announced_broadcast`,
     `moq_consume_catalog`). Prefer following once in moq-ffi and moq-c over
     duplicating it in every language wrapper. If following needs new API,
     report it per the Cross-Package Sync table.
   - JS: `js/net` doesn't upgrade yet (the
     [JavaScript half](/quest/m1/transport-upgrade/js.md) of the upgrade
     line), but a Rust relay's Restart reaches JS consumers. Confirm
     `@moq/watch` and the js-native interop subscriber follow it.
   - The interop clients under `test/interop/clients`, where they resolve a
     broadcast themselves.
3. Regression: land the reproduction above in `websocket_upgrades_to_quic`
   or a sibling, with the clock under control rather than a wall-clock wait
   for the hold. It asserts that a request through the old handle made
   right after the upgrade, before the `Restart` is delivered, does not fail
   with `Unroutable`. It also asserts that the announce consumer then sees
   `Restart` for `cam`, and that following it (a fresh `routed_broadcast`)
   serves the new track. Add the same case on an epoch route, where the old handle
   keeps working and no `Restart` is sent. Then run `just test interop --all`.
   The race can't be forced locally (a fast machine lets QUIC win), so the
   unit test carries the proof.

If a failing client still leaves its connection to idle out, fix that
client's error path to close its session, as a separate commit. The C
client's `fail()` and the C++ client on a consumer error both look suspect.

Public API: the upgrade's `Restart` already exists. What changes is when
an old handle retires relative to it, and that first-party consumers follow
it. Consumer-follow API in moq-ffi and moq-c only if the audit needs it;
report it in the PR. Wire: none.

## Related

- [Transport upgrade](/quest/m1/transport-upgrade/README.md) - its "epochless routes signal Restart" decision, which this quest implements for the Rust upgrade
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - #5294's retired-request rule and the players that already follow `Restart`
- [#5294](https://github.com/moq-dev/moq/pull/5294) - refuse new requests from retired sources, the commit that exposed this
- [#4180](https://github.com/moq-dev/moq/pull/4180) - the WebSocket-to-QUIC upgrade
