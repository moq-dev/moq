# [M] A WebSocket-to-QUIC upgrade keeps a client's broadcasts usable

## Goal

A client whose session comes up over the WebSocket fallback and then
upgrades to QUIC keeps using the broadcasts it already resolved: a new track,
TRACK_INFO, subscribe, or FETCH through a handle taken before the upgrade
succeeds, on default (epochless, lite-06) sessions as well as lite-07. Interop's
full matrix on `main` stops failing with `unroutable`, and its idle-out
failures go away with it.

Non-goals: stitching two different publisher instances. A real restart
(a new epoch, or an epochless route replaced by a different publisher) still
retires old handles, as #5294 decided.

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

Reproduced while planning, on `main` at 079d7d8d4. Add an `audio` track
to `websocket_upgrades_to_quic` and subscribe to it through `cam` after the
upgrade: the subscribe ends with `Unroutable`. With
`RequestValidity::check` forced to pass, the same subscribe succeeds. The
local `just test interop` matrix passes, because QUIC wins there.

Onset matches. The first Interop run with `unroutable` started at 20:09 UTC on
2026-10-11. The last red runs before it failed for other reasons (`media`,
`ts`). Runs before #5294 have no `unroutable`. It still passes sometimes,
whenever QUIC wins the race.

Interop's PR trigger only fires on harness paths, so the nightly and
harness-only PRs like #5381 are the only places this shows up. Users hit it
too: `moq export` on a default connection exits whenever WebSocket wins
briefly.

Open decision (maintainer): what should a self-initiated upgrade mean for
epochless routes? The
[transport upgrade](/quest/m1/transport-upgrade/README.md) plan says
epochless routes "signal Restart and need a fresh follower resolution". Read
literally, every default client must re-resolve after any upgrade.

1. (recommended) The upgrade is a transport swap, not a new instance. The
   upgraded session's re-announcement of a path continues the old session's
   route identity (an update in place, the way `Announced::attached` re-prices),
   unless the peer itself sends a `Restart` or End+Start. Content still never
   crosses a real restart: if the relay's own route changed, it announces
   that on both sessions. This is the smallest change and keeps the upgrade
   invisible, which is why it exists.
2. Upgrades are restarts, and every first-party consumer (`moq export`,
   `moq subscribe`, moq-ffi/moq-c consumers, the interop clients) follows
   `Restart` and re-requests after `Unroutable`. Consistent with the letter
   of the epoch line, but every application pays for a transport detail.
3. Don't upgrade sessions whose routes are epochless. Not decidable per
   session (epochs are per route), and default clients would stay on
   WebSocket.

Whichever is chosen, land the reproduction above as the regression, on an
epochless route and on an epoch route. Check whether the
relay hop has the same problem when a publisher upgrades: the relay's lite
subscriber builds the same validity for the publisher's session. Then run
`just test interop --all` until it passes (the race can't be forced locally;
a fast machine usually lets QUIC win, so the unit test carries the proof).

Afterwards, if a failing client still leaves its connection to idle out,
fix that client's error path to close its session (the C client's `fail()`
and the C++ client on a consumer error both look suspect), as a separate
commit.

Public API: none expected for option 1. Wire: none.

## Related

- [Transport upgrade](/quest/m1/transport-upgrade/README.md) - its "epochless routes signal Restart" decision is the one this quest revisits
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - #5294's retired-request rule, which must keep holding for real restarts
- [#5294](https://github.com/moq-dev/moq/pull/5294) - refuse new requests from retired sources, the commit that exposed this
- [#4180](https://github.com/moq-dev/moq/pull/4180) - the WebSocket-to-QUIC upgrade
