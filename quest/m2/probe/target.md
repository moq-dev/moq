# [M] Session probe target

## Goal

A moq-net session and a `js/net` connection each keep one probe target: the
larger of its local target and the peer's PROBE target. The local target sums
what the session wants to send (its publisher reservations) and what it wants
to receive (its viewer handles), so several broadcasts on one connection add
up without the application coordinating them. The session advertises PROBE
`Increase` where a sender exists, sends its viewer target to the peer, and
hands the probe target to whichever sender its transport has. Without a
sender, behavior is today's: `Report`, and nothing probes.

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
- **Peer target.** A sending session probes to the larger of its own target
  and the peer's PROBE target. Each hop is independent: a relay never forwards
  a viewer's target upstream, and the PROBE target stays per connection, with
  no per-track wire field. moq-net reads no catalog.
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

Tests with mocked time, Rust and JS: the target follows reservations being
added, updated, and dropped, demanded or not; viewer handles sum into the
PROBE target sent to the peer; the session probes to the larger of its own
and the peer's target; `Increase` is advertised only with a sender; opting
out sends no target and probes nothing.
