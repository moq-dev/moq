# [XL] A preempted group pauses and continues in lite-08, by reset or by takeover

## Goal

In lite-08, a group that loses its stream to a higher-priority one is paused
instead of dropped, and a later stream continues it. It loses the stream one
of two ways, designed together: the publisher resets the lowest-priority
Group Stream to free stream credit, or, when that stream's sent data is all
acknowledged, the new group takes the stream over at once with an in-band
marker and needs no credit. lite-07 and older, and moq-transport, keep the
reset as a final drop that the reader FETCHes around.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): ✅ paused
groups and continuations leave lite-07
([YIELDED deferred](/quest/m2/yield-defer.md)) and are designed again here,
with takeover, as one lite-08 feature (rejected: keep YIELDED in lite-07 and
add takeover later; keep it but stop pausing on a reset without a code).
lite-07 would have frozen the half that still needs a new stream per
continuation, on a signal browsers lose. The decisions below still hold
where they don't conflict with these, with "lite-07" and "a later lite
version" read as lite-08:

- **The marker needs a new name and encoding.** The 2026-11-12 pick below
  made takeover's GROUP_END a FRAME whose `Payload Length` is 0, which
  pauses the group. lite-07 now freezes a GROUP_END with that name on that
  reserved value meaning the opposite: the group is whole, and here is its
  exclusive end timestamp
  ([time addressing](/quest/m2/time-addressing/lite07.md)). Recommended: a
  distinct marker (say GROUP_PAUSE), laid out so one zero `Payload Length`
  leads to either; lite-08 is a new version, so it may re-lay what follows
  the zero.
- **The pause must not hang on a reset's error code.** Chromium loses a
  reset that lands right after stream data and Firefox loses its code
  ([browser reset reports](/quest/m4/browser-reset-reports.md)), which is
  why lite-07-wip's draft told a subscriber to pause on any reset without a
  code, and only JS did. Recommended: the publisher says a group is paused
  on the Subscribe Stream, or with the in-band marker, and Rust and JS read
  the same signal.
- **A continuation should not resend.** lite-07-wip's continuation restarted
  at the reset stream's own `Frame Start`, since a reset can discard frames
  the transport acknowledged, so the reader skipped what it held. Weigh a
  continuation that starts where the reader says it stopped, which is what
  a FETCH from a frame index already is.
- **Tracks carrying moq-transport object records** never yielded in
  lite-07-wip. Say what they do here.
- **Measure first.** Before either half is built, measure a lossless reader
  of more tracks than the stream cap: how often a group is preempted, and
  what a drop plus FETCH (lite-07's behaviour) costs against a continuation.
  The 2026-11-13 decision below asked this of takeover's round trip; it now
  covers both halves. If the drop plus FETCH is close, delete this quest.

What lite-07-wip built is in history to start from:
[moq-dev/moq#8545](https://github.com/moq-dev/moq/pull/8545) (YIELDED,
paused groups, continuations, `SubscribeAccounting.tla`),
[moq-dev/moq#8673](https://github.com/moq-dev/moq/pull/8673) (the browser
workarounds), and
[moq-dev/moq#8843](https://github.com/moq-dev/moq/pull/8843) (yielding on
relay links). The `test/stream-credit` harness stays on `main`, checking
drops.

Decided 2026-11-13 (maintainer, /quest-run): ✅ move this quest from m1 to
m3 (rejected: keep it in m1 before lite-07 finalizes; delete it).
[moq-dev/moq#8545](https://github.com/moq-dev/moq/pull/8545)'s YIELDED reset
already gave stream credit to the highest-priority groups under a browser's
~100-stream cap (as the plain reset still does after 2026-11-16); takeover
saves only the ~1 RTT a reset waits for
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

Public API: `StreamError::Yielded`, or whatever replaces it, returns to Rust
and `@moq/net`. Wire: lite-08 paused groups, continuations, a takeover
marker, and multi-group streams.

## Related

- [YIELDED deferred](/quest/m2/yield-defer.md) - removes paused groups and continuations from lite-07
- [YIELDED reset](https://github.com/moq-dev/moq/pull/8545) - the lite-07-wip design this starts from
- [The lite-07 wire addresses groups by start](/quest/m2/time-addressing/lite07.md) - freezes the GROUP_END this must not collide with
- [Finalize lite-07](/quest/m2/lite07-finalize.md) - freezes lite-07 without takeover or continuations
