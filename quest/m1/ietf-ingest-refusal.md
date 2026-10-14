# [S] IETF ingest refuses a malformed group

## Goal

A moq-transport peer's malformed group is refused with STOP_SENDING instead of
being dropped at debug while the handler returns `Ok(())`.

## Plan

Interop on 2026-10-12 found the catch-all `Err(err)` arm after `GroupIngest`
in `rs/moq-net/src/ietf/subscriber.rs` logs at debug and returns `Ok(())`. Gaps
no longer reach it (#5233 returns Unsupported), but these still do:

- a malformed object extension block (`Decode`),
- a FIN before the declared payload ends (`WrongSize`),
- a second stream for a live group (`Duplicate`),
- an oversized frame or group (`FrameTooLarge`, `GroupTooLarge`).

Decided 2026-10-12:

- **Narrow the catch-all.** Route the peer's faults (`Decode`, `WrongSize`,
  `Duplicate`, `FrameTooLarge`, `GroupTooLarge`) to the refusing arm so the
  dispatcher sends STOP_SENDING. Peer resets, transport errors and Cancel stay
  at debug.
- One regression test per case through the IETF dispatch loop, each failing
  without its fix.

Decided 2026-10-13: the planned track abort on a subgroup other than 0 or a
datagram past object 0 is cancelled.
[moq-transport passthrough](/quest/m1/ietf-extension/README.md) carries both
instead.

## Related

- [#5371](https://github.com/moq-dev/moq/issues/5371) - closed: its gap case was fixed by #5233, and it pointed at the catch-all this narrows

- [Malformed moq-transport input](/quest/m1/ietf-malformed-close.md) - session-level malformed control input
- [Object fields](/quest/m1/ietf-extension/objects.md) - changes the same receive paths
