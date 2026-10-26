# [M] Lite-05/06 timestamps pass through

## Goal

A timed track received over moq-lite-05/06 keeps its frame and datagram
timestamps, and a relay forwards them over every version, so a lite-06
reader of a timed feed (such as a stats feed) sees timed frames instead of
untimed ones. Partly reverses
[moq-dev/moq#6150](https://github.com/moq-dev/moq/pull/6150), which drops
every lite-05/06 timestamp a receiver reads.

## Plan

Decided 2026-10-25 (maintainer): "We dropped lite timestamps because they
defaulted to wall clock timestamps, but we should pass them through instead."

- A track received over lite-05/06 is timed and marked crate-private
  `unchecked`, like a moq-transport one: its groups skip the
  [moq-dev/moq#5803](https://github.com/moq-dev/moq/pull/5803) rewind check
  on ingress, because an untimed origin stamps send times at serve time and
  an older group fetched later would look like a rewind.
- Unchecked tracks, lite-05/06 and moq-transport alike, go out with their own
  timestamps over every version: lite-05/06, lite-07, and moq-transport. A
  moq-transport track served to a lite-05/06 peer stops being stamped with
  send times.
- A lite-07 receiver keeps checking every received group (no wire flag), so a
  send-time origin's out-of-order group may be refused downstream; rewinds
  are unsupported, and the cost was accepted.
- A lite-05/06 sender serving an untimed track keeps the published rule:
  timescale 1000 and each payload's send time in milliseconds. Receivers now
  pass that through as the track's time.

Update the draft's lite-05/06 receiver text and changelog,
`doc/concept/moq-lite.md`, and the `MIGRATE.md` entries #6150 added, in Rust
and `@moq/net` alike. Test a lite-06 hop between two relays: a timed track
arrives timed with its values, and an out-of-order fetched group from an
untimed origin is accepted on lite-05/06.

Public API: none (crate-private `unchecked`). Wire: none for lite-05/06;
receiver behavior changes, and the lite draft text says so.
