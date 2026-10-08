# [S] Demand polls never lose a wake

## Goal

A reader that arrives or leaves between a demand poll and its re-read never
leaves the waiter without a wake. Today a relay front's driver can miss a
track's `Unused` edge: the copy keeps its upstream subscription for nobody,
and the front never retires, with nothing to bound it on a quiet
single-track front. In the other direction a reader stalls on a parked track
until the linger expires. `broadcast::Demand::poll_demand` has the same shape
and is fixed with it, and a closed broadcast never spins the front's holder
edge.

## Plan

Facts from `main` at 7c6b6afc1 (line numbers at time of writing):

- `serve_front`'s wait closure (`rs/moq-net/src/model/origin.rs:2728-2735`)
  calls `io.weak.poll_unused` or `poll_used`, then steps only if
  `io.weak.is_used() != io.used`. `TrackWeak::poll_used` and `poll_unused`
  (`track.rs:2550-2558`) discard the result, and kio
  (`rs/kio/src/weak.rs:168-188`) returns `Ready` without registering when
  the edge is already met. With `used` true: no consumers, so `Ready` without
  a registration; a reader arrives, so `is_used()` is true and the closure
  returns `Pending`; the reader leaves and nothing wakes the driver.
- `Step::Demand` (`origin.rs:2841-2857`) re-reads `is_used()` and can emit a
  duplicate `Used` or `Unused`.
- `broadcast::Demand::poll_demand` (`broadcast.rs:136-147`, `887-904`):
  `register_demand` drops each track's `Ready`, then recomputes `is_used()`.
- On #5054's head 3b6d36f67, `poll_held` and `poll_unheld`
  (`broadcast.rs:386-393`) map kio's `Ready(Err(Closed))`, returned before
  registering, to `Ready`, and `Step::Holders` `continue`s while
  `is_held() == held`. A closed broadcast would livelock the driver in
  `kio::wait`, which has no budget. Not reachable today: the only closers of
  the front's broadcast, `Action::End` and `Retire` through `End`, end the
  driver in the same pass, and `FrontTask` owns the producer.

Decided 2026-10-08:

- m0: it undermines the idle-front retire (#5054) and leaks an upstream
  subscription.
- Covers `broadcast::Demand` too: same pattern, and publishers rely on its
  edges being clean.
- Reuse the handler shape Idle fronts uses for the holder edge (`poll_held`,
  `poll_unheld`): the closure steps on `Ready`, and the handler re-reads and
  polls again (registering) when nothing changed. Not its wrappers, which
  map a closed channel to `Ready` on every pass.
- Fold the holder-edge hardening in, since this rewrites the same closure: a
  closed broadcast ends the front (or skips the holder edge) instead of
  polling it again, so safety is local rather than resting on who owns the
  producer.
- Distinct from kio's level-only demand, which
  [Front deadline index](/quest/m1/front-deadline-index.md) scopes out: there
  a reader that comes and goes between polls skips restarting the linger, but
  no wake is lost.

Pitfalls:

- A track's `Ready(Err(Closed))` is an edge only while the track is recorded
  used. Closing flips `is_used()` to false (an upstream refusing an active
  track ends it this way), so it owes one final `Unused`, or it stays used
  and its front never retires. After that it is no edge, or an ended unused
  track spins.
- `broadcast::Demand` has no handler to poll again: publishers await
  `poll_demand` directly, so the fix lives inside the poll. Prefer deciding
  from the per-track poll results over re-reading `is_used()`: every track
  `Ready` meets `unused`, any `Ready(Ok)` meets `used`, and otherwise the
  tracks in the way hold registrations. Never retry just because a track was
  `Ready`: with one track unused and another still read, that spins until
  the last viewer leaves.

Verification: a loom model beside Idle fronts'
`a_request_never_joins_a_retiring_front` in `rs/moq-net/tests/loom.rs`, with
a reader thread taking and dropping a track around the driver's poll, failing
without the fix; the same for `Demand`. Also cover a track that closes while
used (one `Unused`) and one that closes already unused (no spin), and for
`Demand` two tracks: `Pending` while only one is read, done once its last
reader leaves. A front whose broadcast closes while unheld must not spin;
close it from a test, since no production path does yet. The simulated
network can't hit a window inside a single poll.

Public API: none. Wire: none.

## Related

- [Front deadline index](/quest/m1/front-deadline-index.md) - later replaces this closure with per-track wakes, which must keep the fix
