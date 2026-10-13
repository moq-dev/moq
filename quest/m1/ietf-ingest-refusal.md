# [S] IETF group ingest refuses loudly

## Goal

A moq-transport publisher whose groups moq-net cannot represent gets a loud
refusal instead of a silently truncated track. A subgroup ID other than 0
or a datagram past object 0 aborts the track with Unsupported, logged at
warn. A peer's malformed group is refused with STOP_SENDING instead of being
dropped at debug while the handler returns `Ok(())`.

## Plan

Subgroups and object ID gaps stay unsupported on purpose
([#4926](https://github.com/moq-dev/moq/issues/4926), declined). Interop on
2026-10-12 (a LOC publisher at draft-18, one GOP per group and one object per
frame, and a draft-22 test suite) found that the refusal is quiet. In
`rs/moq-net/src/ietf/subscriber.rs`:

- `recv_group` refuses a subgroup other than 0 with Unsupported, which
  resets only that stream, so subgroup 0 is forwarded as if whole.
- `recv_datagram` drops a datagram past object 0 at debug and returns
  `Ok(())`. Object 0 is forwarded with `end_of_group` set, so viewers get
  truncated groups and no error.
- The catch-all `Err(err)` arm after `GroupIngest` logs at debug and returns
  `Ok(())`. Gaps no longer reach it (#5233 returns Unsupported), but a
  malformed object extension block (`Decode`), a FIN before the declared
  payload ends (`WrongSize`), a second stream for a live group
  (`Duplicate`), and an oversized frame or group (`FrameTooLarge`,
  `GroupTooLarge`) still do.

Decided 2026-10-12:

- **Abort the track** on the first subgroup other than 0 or datagram past
  object 0, with Unsupported at warn. The subscriber sees an error, not
  partial media.
- **Narrow the catch-all.** Route the peer's faults (`Decode`, `WrongSize`,
  `Duplicate`, `FrameTooLarge`, `GroupTooLarge`) to the refusing arm so the dispatcher sends STOP_SENDING.
  Peer resets, transport errors and Cancel stay at debug.
- One PR for both, since they share the error routing.
- **Docs.** Record in the deliberate deviations of
  `doc/concept/standard.md` that per-subgroup and per-datagram publisher
  priority is rewritten to the track's (the model's `group::Info` has no
  priority), beside the existing dropped-properties entry, and that groups
  past subgroup 0 or object 0 abort the track.
- One regression test per case through the IETF dispatch loop, each failing
  without its fix.

## Related

- [#5371](https://github.com/moq-dev/moq/issues/5371) - closed: its gap case was fixed by #5233, and it pointed at the catch-all this narrows

- [Malformed moq-transport input](/quest/m1/ietf-malformed-close.md) - session-level malformed control input
