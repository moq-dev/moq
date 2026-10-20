# [L] Session probe target

## Goal

A moq-net session and a `js/net` connection each keep one target per
direction, so several broadcasts on one connection add up without the
application coordinating them. The send target is the larger of what the
session wants to send (the sum of its publisher reservations) and the peer's
PROBE target; the session hands it to whichever sender its transport has and
advertises PROBE `Increase` where one exists. The receive target is the sum of
its viewer handles, sent to the peer as its PROBE target. The two never mix: a
session that publishes and watches doesn't probe its uplink for what it
watches. Without a sender, behavior is today's: `Report`, and nothing probes.

Each viewer handle also reads a grant: its share of the session's receive
estimate, so several broadcasts watched over one connection each select
against their own share instead of the whole downlink.

## Plan

Decided with the maintainer on 2026-10-17 (see the
[epic](/quest/m2/probe/README.md)):

- **Publisher side: reuse the reservation.** The local send target is the sum
  of the `max` ceilings of every live `Reservation` on the connection's
  `bandwidth::Allocator`, including a disabled rung's and an undemanded
  track's, because [rung disable](/quest/m2/probe/simulcast-rung-disable.md)
  needs recovery that doesn't depend on demand: viewers deselect a disabled
  rung, so its demand goes away. Dropping a reservation removes it from the
  sum. No new publisher handle, and publishers need no catalog reader.
  Rejected: summing only demanded reservations, and an opt-in flag per
  reservation.
- **Viewer side: a handle per broadcast.** The subscriber session hands out a
  reservation-like handle carrying a `max`, one per watched broadcast; the
  session sums the live handles and sends the sum as its PROBE target.
  Updating or dropping a handle changes the sum. Rejected: one target the
  application sums, which pushes the multi-broadcast case onto every app.
  [Players](/quest/m2/probe/players.md) hold these handles.
- **Viewer grants.** Decided with the maintainer on 2026-10-19: a viewer
  handle carries a grant the way a publisher `Reservation` does, splitting the
  receive estimate across live handles max-min fair by `max`, the split the
  send `Allocator` applies within one priority tier
  (`rs/moq-net/src/model/bandwidth.rs`). A handle asking for less than an even
  cut takes all of it and leaves the rest to the others. Today every
  `<moq-watch>` on a pooled connection selects against 0.8x the whole
  `estimatedRecvRate`, so N watchers over-select about N times. Rejected: each
  watcher measuring its own subscription's rate, which stays app-limited and
  can't see headroom, and dividing the estimate by the watcher count, which
  ignores each handle's `max`.
  Handles carry no priority: subscription priorities are per track and fixed
  by kind, so no per-broadcast priority exists to divide by. Add one when a
  player needs to favor one broadcast over another.
- **Peer target.** A sending session probes to the larger of its own target
  and the peer's PROBE target. Each hop is independent: a relay never forwards
  a viewer's target upstream, and the PROBE target stays per connection, with
  no per-track wire field. moq-net reads no catalog. A peer's target is
  untrusted, but it only sets how high to probe: each probe scales the
  current estimate (BBR's 1.25x phase, padding's 2x cluster) and stays under
  congestion control, so a huge target costs no more than one just above the
  estimate. Clamp it to the session's rate cap where one exists
  ([rate grant](/quest/m2/rate-grant.md)) rather than adding a constant.
- **Automatic, opt-out.** Targets are on by default; an application can
  override the session's target or disable probing.
- **The sender seam.** Define the hook a transport implements to probe up to a
  rate, and advertise `Increase` in SETUP only when the session has a sender
  behind it. [Native](/quest/m2/probe/native.md) implements it in moq-quic,
  and [padding](/quest/m2/probe/padding.md) is the fallback for transports
  that don't. This quest only needs a test sender.

Notes for the implementer:

- `js/net` already owns its connection's `Allocator`
  (`js/net/src/connection/reload.ts`), but Rust callers build one from
  `send_bandwidth()` themselves (`rs/moq-cli`, `rs/moq-c`, `rs/moq-ffi`), so
  the session can't see their reservations. Settle who owns the Rust
  allocator, and keep it across reconnects the way the estimate is.
- Rust sessions request PROBE today only while an owned `recv_bandwidth()`
  consumer is held (`doc/concept/standard.md`); the viewer handle may subsume
  that.
- Wire: none. PROBE already carries a target and SETUP the `Increase` level
  (`drafts/draft-lcurley-moq-lite.md`, `drafts/draft-lcurley-moq-probe.md`).
  Report the public API in the PR, mirrored across Rust and JS, and carry it
  through moq-ffi and the bindings per the cross-package sync table.

Tests with mocked time, Rust and JS: the send target follows reservations
being added, updated, and dropped, demanded or not; viewer handles sum into
the PROBE target sent to the peer; the session probes to the larger of its
reservations and the peer's target; a session that publishes and watches
keeps the two directions apart; viewer grants split the receive estimate
max-min fair by `max` and never sum above it; `Increase` is advertised only with a sender;
opting out sends no target and probes nothing.

The sums fan out over reservations and viewer handles, so add a benchmark
sweeping both counts independently, wired into the benchmark CI, so an update
that rescans every handle shows up as a slope.
