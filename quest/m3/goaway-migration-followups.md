# [S] A migrated session closes early only once its replacement has synced, in Rust and JS

## Goal

After a GOAWAY migration, the Rust connect policy (moq-net's `Reconnect`, which moq-tokio's `Connection` drives) closes the idle predecessor only once the replacement's announcements have synced, so a broadcast nobody subscribes to never reads as ended and started again during the switch. The JS `Connection` closes an idle predecessor early the same way, instead of holding it for `goaway.handover`.

## Plan

Follow-ups of [moq-dev/moq#7462](https://github.com/moq-dev/moq/pull/7462) (merged), which added moq-net's `Session::poll_idle` (now crate-private; ready once the session carries no subscription or fetch in either direction) and closes a migrated predecessor once it is idle, instead of waiting out `connect.goaway.handover` (10 s by default). Since [moq-dev/moq#7904](https://github.com/moq-dev/moq/pull/7904) that close lives in moq-net's connect policy: `Predecessor::poll_idle` in `rs/moq-net/src/reconnect.rs` (about line 1190 as of a86433043).

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): both parts in one quest, since the JS half should copy the Rust half's final rule rather than #7462's.

- **Hold for the replacement's announce sync.** Today the idle predecessor can close as soon as the replacement connects, before the replacement's announcements reach either side, so an unsubscribed broadcast briefly reads as ended then started. Gate the close on both idle and the replacement's announce sync. First check whether [moq-dev/moq#8375](https://github.com/moq-dev/moq/pull/8375), which reports a let-go as offline rather than END, already hides the flap from listeners; if it does, narrow this half to what still shows. That needs a signal moq-net doesn't expose yet: a per-session "announcements synced" readiness (lite-07's announce sync, and the IETF equivalent or a bound where the version can't say). Keep the handover cap as the upper bound.
- **JS early close.** `js/net/src/connection/reload.ts` holds the predecessor to `goaway.handover` (the `Draining` class and its `handover(...)` cap, about lines 815 and 1061 as of a86433043). Add an idle signal on `Connection.Session` mirroring `Session::poll_idle` (subscriptions and fetches in either direction; announcements, track info, PROBE, and AUTH don't count), the same announce-sync gate, and a retire in `Reload` that closes the predecessor when both hold.
- Test both on simulated time over the mock transport, as `rs/moq-net/src/session/tests.rs` does for `poll_idle`: a broadcast announced on both sessions and subscribed by nobody never shows an end across the migration, and a busy predecessor still waits for its readers.

Public API: a moq-net readiness for the announce sync, `Connection.Session`'s idle signal in `@moq/net`, and their mirrors. Wire: none.

