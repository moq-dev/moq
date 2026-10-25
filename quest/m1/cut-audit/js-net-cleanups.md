# [S] Small @moq/net divergences from Rust are fixed

## Goal

`@moq/net` matches Rust on four small behaviors the pre-cut audit found:
lite-03/04 sessions get probe estimates, a group's `consume()` starts at
frame 0, `Connection.accept()` has the same handshake deadline and
close-on-failure as `connect()`, and an empty JSON frame fails instead of
reading as EOF. Dead code found alongside is gone.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670.

- **No PROBE on lite-03/04** (reproduced, inherited):
  `js/net/src/lite/subscriber.ts:1753` waits on `#peerProbeLevel`, but
  lite-03/04 have no SETUP stream, so it never resolves and ABR gets no
  estimates on moq-lite-04, which is in the default list. Gate on
  `hasSetupStream(version)` like Rust (`rs/moq-net/src/lite/subscriber.rs:1453`).
  Add a two-`Connection` test; the unit tests build `Subscriber` without
  `peerSetup`, which hid this.
- **`group.Producer.consume()` shares a cursor** (reproduced, inherited):
  `group.ts:269` returns `makeConsumer(this.#state)`, so a later reader
  starts where an earlier one left off. Return an independent handle, as
  `mirror()` does; `#cachedLargest` (`track.ts:1262`) needs only
  `frameCount`.
- **`Connection.accept()` has no deadline** (source-inferred, inherited):
  `connection/accept.ts` waits forever for SETUP and leaves the transport
  open on a malformed one. Reuse `connectTransport`'s race-and-close, with
  additive `timeout`/`signal` options, matching Rust's `server.rs`
  `Error::Timeout`.
- **Empty JSON frame reads as EOF** (reproduced, inherited): `group.ts:993`
  and `track.ts:2821` treat an empty frame as end of stream, so `zod.read`
  stops silently; `group.Consumer.readBool` skips validation. Fail loud.
- **Dead code:** the `timescale` Signal wait in `lite/subscriber.ts:1615`
  and `:1705` can never block (it is set before the entry registers); make
  it a plain number. `Reload.announced()` and `stats()`
  (`connection/reload.ts:660`) appeared to have only test consumers (the
  public `Connection` in `pool.ts` has its own), and `announced()` registers
  a root cleanup per call that is never released, the pile-up `pool.ts:341`
  already fixed. Confirm no other consumer, then delete both; otherwise fix
  the leak.

Public API: `accept()` gains optional `timeout`/`signal`; `Reload.announced()`
and `stats()` are removed if `Reload` is exported. Wire: none.

## Related

- [JS reload config](/quest/m1/js-reload-config.md) - the other `Reload` change before the cut
