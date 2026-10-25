# [M] Viewer probe targets and grants

## Goal

A moq-net session and a `js/net` connection each keep a receive target: the
sum of their viewer handles, sent to the peer as the PROBE target. A sending
session probes to the larger of its own send target
(`bandwidth::Allocator`'s; in `js/net`, the one [padding](/quest/m2/probe/padding.md) adds) and the
peer's PROBE target, and advertises PROBE `Increase` where it has a
sender. The two directions never mix: a session that publishes and watches
doesn't probe its uplink for what it watches.

Each viewer handle also reads a grant: its share of the session's receive
estimate, so several broadcasts watched over one connection each select
against their own share instead of the whole downlink.

## Plan

Split from the send target by the maintainer on
2026-10-20: m1 native rung recovery needs only the send target, while these
handles' consumers ([players](/quest/m2/probe/players.md) and the
[viewer up-switch](/quest/m2/probe/viewer-upswitch.md)) are in m2.

Decided with the maintainer on 2026-10-17 (see the
[epic](/quest/m2/probe/README.md)):

- **A viewer handle per broadcast.** The subscriber session hands out a
  reservation-like handle carrying a `max`, one per watched broadcast; the
  session sums the live handles and sends the sum as its PROBE target.
  Updating or dropping a handle changes the sum. Rejected: one target the
  application sums, which pushes the multi-broadcast case onto every app.
  Players hold these handles.
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
  congestion control. Repeated cycles can still walk the estimate up to the
  path's capacity, so a huge target costs the redundant bytes of probing that
  far, held to native probing's overhead budget (redundant bytes at most
  10% of unique bytes, [moq-dev/moq#6635](https://github.com/moq-dev/moq/pull/6635)); a peer that subscribes to more media can already cost the
  sender that much. Clamp it to the session's rate cap where one exists
  ([rate grant](/quest/m2/rate-grant.md)) rather than adding a constant.
- **`Increase` only with a sender.** SETUP advertises `Increase` only when
  the session has a sender behind the seam the send target defines: native
  probing on moq-quic, padding in the browser. A browser session on a version
  [padding](/quest/m2/probe/padding.md) refuses (lite-05, lite-06, IETF
  drafts 14 to 17) has none, so it keeps `Report`.
- **Automatic, opt-out.** Targets are on by default; an application can
  override them or disable probing.

Notes for the implementer:

- Rust sessions request PROBE today only while an owned `recv_bandwidth()`
  consumer is held (`doc/concept/standard.md`). If the viewer handle subsumes
  that, migrate those callers in the same change; a held consumer never
  silently stops requesting PROBE.
- Wire: none. PROBE already carries a target and SETUP the `Increase` level
  (`drafts/draft-lcurley-moq-lite.md`, `drafts/draft-lcurley-moq-probe.md`).
  Report the public API in the PR, mirrored across Rust and JS, and carry it
  through moq-ffi and the bindings per the cross-package sync table.

Tests with mocked time, Rust and JS: viewer handles sum into the PROBE target
sent to the peer; the session probes to the larger of its reservations and
the peer's target; a session that publishes and watches keeps the two
directions apart; viewer grants split the receive estimate max-min fair by
`max` and never sum above it; `Increase` is advertised only with a sender, and never by a browser session
on a version padding refuses;
opting out sends no target.

The sum fans out over viewer handles, so add a benchmark sweeping their count,
wired into the benchmark CI, so an update that rescans every handle shows up
as a slope.
