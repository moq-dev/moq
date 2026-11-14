# [L] A higher-priority group takes over an acknowledged stream in a later lite version

## Goal

In a lite version after lite-07, when an open waits for credit and a lower-priority group stream's
sent data is all acknowledged, the new group takes that stream over at once:
the group stream ends one group with GROUP_END and starts another, needing no
credit. The group that lost its stream pauses and a later stream continues
it, as a YIELDED reset's group already does. moq-transport, lite-07, and older lite
versions keep reset only.

## Plan

Decided 2026-11-13 (maintainer, /quest-run): ✅ move this quest from m1 to
m3 (rejected: keep it in m1 before lite-07 finalizes; delete it).
[moq-dev/moq#8545](https://github.com/moq-dev/moq/pull/8545)'s YIELDED reset
already gives stream credit to the highest-priority groups under a browser's
~100-stream cap; takeover saves only the ~1 RTT a reset waits for
MAX_STREAMS, at the cost of a new wire message (GROUP_END) and multi-group
streams in Rust and JS, and lite-07's finalization does not need it.
Takeover comes in a later lite version only if measurements show that round
trip matters; the decisions below still hold, with "lite-07" read as that
version.

Decided 2026-11-04 (maintainer): only a stream whose sent data is all
acknowledged qualifies, since a QUIC stream is ordered and the new group's
bytes would otherwise queue behind the old group's retransmissions.

Decided 2026-11-12 (coordinator on the maintainer's behalf, taking
[moq-dev/moq#8545](https://github.com/moq-dev/moq/pull/8545)'s
recommendations): ✅ GROUP_END is a FRAME whose `Payload Length` is 0
(rejected: a new Group Stream type with a 1-byte type per frame; no in-band
marker, reset only).

Decided 2026-11-12 (maintainer, scope of
[moq-dev/moq#8545](https://github.com/moq-dev/moq/pull/8545)): ✅ split
(rejected: one PR resized XL). #8545 lands the lite-07 FRAME reorder
(`Payload Length` first, 0 malformed for now), the YIELDED (0x3C) reset that
pauses a group, paused groups and continuations in Rust and JS, the
`SubscribeAccounting.tla` model, and the resumes-without-loss test, with no
takeover text in the draft. This quest owns takeover itself:

- The draft's takeover text: a publisher out of credit writes GROUP_END after
  a whole FRAME, then the new Group's GROUP message, on the same stream; it
  SHOULD NOT take over a stream before every byte sent on it is acknowledged;
  a stream carrying the `ietf` extension's object records is never taken
  over. A GROUP_END on a Fetch Stream, or as a Group Stream's last bytes, is
  malformed. The group a GROUP_END ends is paused, and its continuation starts
  at the index after its last frame before the GROUP_END.
- Rust: a handle on `transport::open::Lines` that hands a listed stream to a
  waiting open, tried before `boxed::Listed::preempt`, and a stream handoff
  between two `GroupServe` tasks at a frame boundary.
- JS: the same on `OpenStream` (`js/net/src/open.ts`).
- Subscriber: `GroupRecv` and its JS counterpart parse more than one group on
  a stream, each GROUP message after a GROUP_END binding the stream to its own
  subscription and group.
- Tests: a browser over WebTransport and a Rust relay, more live groups than
  the cap, the higher-priority group starting without waiting a round trip.
  The epic's 300-track end-to-end test reuses this harness.

Implementation guidance (recommended): take over only a stream whose writer is
parked between frames (waiting for its group's next frame) and fully
acknowledged; a stream mid-frame falls back to the YIELDED reset, so no frame
waits on a handoff. `SubscribeAccounting.tla` gains GROUP_END the way it gained
YIELDED.

Decided 2026-11-13 (maintainer, the pick round of
[moq-dev/moq#8545](https://github.com/moq-dev/moq/pull/8545)): ✅ the new
Group may belong to any subscription on the session (rejected: only the
subscription whose stream it takes over), since stream credit is
session-wide and the waiting higher-priority group is usually another
track's. `GroupRecv` rebinds the stream at each GROUP message.

Public API: none. Wire: GROUP_END and group stream takeover in a later lite
version.

## Related

- [YIELDED reset](https://github.com/moq-dev/moq/pull/8545) - the reset-based priority scheduling this would shave a round trip off
- [Finalize lite-07](/quest/m2/lite07-finalize.md) - freezes lite-07 without takeover
