# [M] Session send target

## Goal

A moq-net session and a `js/net` connection each keep one send target: the
sum of its publisher reservations, so several broadcasts on one connection add
up without the application coordinating them. The session hands it to
whichever sender its transport has, which probes up to it. Without a sender,
behavior is today's: nothing probes. This is what the native
[rung disable](/quest/m1/simulcast-rung-disable.md) needs to resume a stopped
rendition; targets a peer asks for are the m2
[viewer targets](/quest/m2/probe/viewer-target.md) quest.

## Plan

Decided with the maintainer on 2026-10-20: this moved to m1 with
[native probing](/quest/m1/probe-native.md) and the native rung disable, so a
native publisher's stopped rendition can resume in the next breaking release.
Later that day the maintainer split it: m1 keeps only the send target native
rung recovery needs, and viewer handles, viewer grants, the peer's PROBE
target, and advertising `Increase` moved back to m2 as
[viewer targets](/quest/m2/probe/viewer-target.md), whose consumers (players
and the viewer up-switch) are there.

Decided with the maintainer on 2026-10-17 (see the
[epic](/quest/m2/probe/README.md)):

- **Reuse the reservation.** The send target is the sum of the `max` ceilings
  of every live `Reservation` on the connection's `bandwidth::Allocator`,
  including a disabled rung's and an undemanded track's, because rung disable
  needs recovery that doesn't depend on demand: viewers deselect a disabled
  rung, so its demand goes away. Dropping a reservation removes it from the
  sum. No new publisher handle, and publishers need no catalog reader.
  Rejected: summing only demanded reservations, and an opt-in flag per
  reservation.
- **Automatic, opt-out.** The target is on by default; an application can
  override it or disable probing.
- **The sender seam.** Define the hook a transport implements to probe up to
  a rate. [Native](/quest/m1/probe-native.md) implements it in moq-quic, and
  [padding](/quest/m2/probe/padding.md) in the browser. This quest only needs
  a test sender. Probing to a local target is invisible on the wire, so SETUP
  keeps advertising `Report`; `Increase` promises to probe to the peer's
  target, which viewer targets adds.

Notes for the implementer:

- `js/net` already owns its connection's `Allocator`
  (`js/net/src/connection/reload.ts`), but Rust callers build one from
  `send_bandwidth()` themselves (`rs/moq-cli`, `rs/moq-c`, `rs/moq-ffi`), so
  the session can't see their reservations. Settle who owns the Rust
  allocator, and keep it across reconnects the way the estimate is.
- Wire: none. Report the public API in the PR, mirrored across Rust and JS,
  and carry it through moq-ffi and the bindings per the cross-package sync
  table.

Tests with mocked time, Rust and JS: the send target follows reservations
being added, updated, and dropped, demanded or not; the session hands it to
its sender; opting out probes nothing; SETUP still advertises `Report`.

The sum fans out over reservations, so add a benchmark sweeping their count,
wired into the benchmark CI, so an update that rescans every reservation
shows up as a slope.
