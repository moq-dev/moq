# [M] A higher-priority group takes over an acknowledged stream on lite-07

## Goal

On lite-07, when an open waits for credit and a lower-priority group stream's
sent data is all acknowledged, the new group takes that stream over at once:
the group stream ends one group and starts another, needing no credit. A
group that lost its stream, by takeover or by the reset fallback (an open
waiting for credit resets the lowest-ranked stream, reported in
SUBSCRIBE_DROP as `TOO_FAR_BEHIND`), resumes from where it
stopped when it is next served, under the duplicate rule of
[moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670). moq-transport
and older lite versions keep reset only.

## Plan

Decided 2026-11-05 (maintainer, /quest-plan): split from "A higher-priority group makes room". Takeover first,
as decided 2026-11-04: only a stream whose sent data is all acknowledged
qualifies, since a QUIC stream is ordered and the new group's bytes would
otherwise queue behind the old group's retransmissions. Add the takeover
wire to `drafts/draft-lcurley-moq-lite.md` before lite-07 finalizes, and
define where a reset or taken-over group resumes.

Choose from the session's open group streams: Rust's
`transport::open::Lines::open_streams` (opened through
`boxed::Session::poll_open_group`) and JS's `lines(quic).streams`, each lowest
ranked first with whether the peer has acknowledged it. Each can already be
reset to make room (Rust `boxed::Listed::preempt`, JS `OpenStream.preempt`);
add the handle that hands a stream to a new group here, tried before the reset.

Tests: a browser over WebTransport and a Rust relay, more live groups than
the cap, the higher-priority group starting without waiting a round trip;
a group that lost its stream resumes without loss.

Decided 2026-11-05 (maintainer, /quest-audit): ✅ the wait on
[moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670), Drop set's own
PR, becomes a `Required` link to Drop set ([moq-dev/moq#8476](https://github.com/moq-dev/moq/pull/8476)), since
resuming a reset group sends a second stream for the same group, which #7670's
hold-the-duplicate rule rewrites.

Public API: none. Wire: lite-07 group stream takeover.

The draft specifies the wire ([Takeover](/drafts/draft-lcurley-moq-lite.md),
GROUP_END, YIELDED): what remains is the Rust and JS publishers taking over
and continuing paused Groups, the subscribers holding and continuing them,
and the tests above.

Decided 2026-11-12 (coordinator on the maintainer's behalf while away, taking
the PR's recommendations; open to revision on review):

- GROUP_END encoding: ✅ lite-07 FRAME leads with `Payload Length + 1`, 0 =
  GROUP_END (rejected: a new Group Stream type with a 1-byte type per frame;
  no in-band marker, reset only).
- Reset fallback: ✅ a new YIELDED (0x3C) reset pauses the group, with no
  SUBSCRIBE_DROP (rejected: TOO_FAR_BEHIND plus the drop, resuming only via a
  fresh group).

Implementation guidance (recommended, not yet settled by review):

- Take over only a stream whose writer is parked between frames (waiting for
  its group's next frame) and fully acknowledged; a stream mid-frame falls
  back to the YIELDED reset, so no frame waits on a handoff.
- A paused group re-enters its subscription's cursor at its own sequence,
  ranked like any other group, so the delivery budget can still expire it
  (then named in SUBSCRIBE_DROP as OLD).
- The subscriber keeps a paused group's `group::Producer` open in its
  subscription entry; a continuation takes it and skips the frames it holds.
- `verify-subscribe`'s `SubscribeAccounting.tla` gains the YIELDED pause and
  the continuation, since both change how a Group Stream accounts for a
  sequence.

## Waiting

- Maintainer: scope. The implementation is about XL, not M: a FRAME codec reorder in Rust and JS (publisher, subscriber, Fetch, corpus), a paused-group state on both sides of both languages, a cross-task stream handoff at a frame boundary, multi-group Group Stream parsing, the `SubscribeAccounting.tla` model, and a new browser-over-WebTransport test harness. Options: (1) split: this quest (resized [L]) lands the FRAME reorder, YIELDED, paused groups and continuations, the model, and the resumes-without-loss test, with the Takeover text moved out of the draft; a new [L] follow-up adds takeover (the `Lines` handle before `Listed::preempt`, JS `OpenStream`, GROUP_END, multi-group streams) and the browser test; (2) keep one PR, resized [XL]. Recommend (1): YIELDED pausing is the machinery takeover builds on, each half keeps the draft matching the code at merge, and each stays reviewable.
