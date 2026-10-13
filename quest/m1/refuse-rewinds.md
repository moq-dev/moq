# [S] A track refuses a timestamp that rewinds

## Goal

A timed track never holds a group whose first frame is earlier than the
previous group's, in Rust and js/net. Writing one fails loudly instead of
being cached and served. Untimed tracks are exempt.

## Plan

Decided by the maintainer on 2026-10-13
([#5446](https://github.com/moq-dev/moq/pull/5446)): never support timestamp
rewinds. Today the model accepts them, and its expiry logic works around
them.

Where a group's start is stamped:

- Rust: `GroupState::stamp` (`rs/moq-net/src/model/group.rs`), called from
  `group::Producer::write_frame` and `write_frames`, sets a group's
  start from its first frame. The track keeps groups by sequence in its
  `lookup` map (`rs/moq-net/src/model/track.rs`).
- JS: `state.timestamp ??= frame.timestamp` in `js/net/src/group.ts` stamps
  the start; the track keeps groups by sequence in its sorted `timeline`
  (`js/net/src/track.ts`).

The rule: when a group's first frame lands, its start must be at or after
the nearest cached, stamped, non-aborted group below it, and at or before
the nearest one above it. Both sides are checked because groups can be
created and received out of sequence order. An equal start is allowed, as
moq-mux's `container::Producer` already allows a keyframe at the previous
group's start. Groups no longer cached are not checked. The write fails
without writing: Rust's `write_frame` and `write_frames` commit the frame
before `stamp` today, so the check runs before any cache, frame, or index
change, and the start is stamped only once it passes. A session receiving such a group aborts the track with
that error and keeps the session.

Cases the rule must cover (from review):

- Only an original start counts. A group received from a later frame
  (`start_at(hdr.frame_start)` in `rs/moq-net/src/lite/subscriber.rs`, or a
  partial fetch) stamps its first *received* frame, which can sit above a
  valid successor's start, since moq-mux lets a keyframe overlap the previous
  group's last frame. A group whose first held frame is not index 0 has no
  known start: it is neither checked nor used as a neighbor's bound.
- The neighbor check and the first stamp are one atomic step under the
  shared track state, taken in the existing track-before-group lock order.
  Groups are written on independent streams, so two unstamped neighbors
  checked under only their own locks could each accept a rewind.
- A JS `GroupProducer` can hold frames before `track.writeGroup` attaches
  it, so its start is stamped with no timeline to check. Check an already
  started group when it is attached, before it is cached or fanned out.

The check costs one ordered neighbor lookup per group, not per frame. The
existing `group_write_frames` benchmark writes into an otherwise empty
track, so add a row that stamps groups swept over the retained group count,
where a scan of `lookup` would show as a slope.

Recommended: every track producer refuses, local or received. The check
sits where the start is stamped, which publishers, relays, and Rust and JS
subscribers all share, so one rule covers every hop. A buggy or foreign
publisher (including a moq-transport peer) fails at the first hop instead
of being forwarded, and every hop's expiry and delivery logic (`reach`, the
drift budget, #5446's delivered floor) can rely on starts never falling.
Alternative: refuse only local writes (`append_group`, `create_group`, and
the JS equivalents) and let received groups through. It is cheaper on the
forwarding path, but relays forward a peer's rewind and every hop must still
tolerate one.

Error: recommended, reuse `TimestampMismatch` (stream code 0x39 on the wire)
and broaden its doc, in Rust, js/net, and the code's description in
`drafts/draft-lcurley-moq-lite.md`, to "a frame's timestamp doesn't fit its
track". No new code, no new public variant. Alternative: a distinct
`TimestampRewind` error with its own stream code, a wire addition that
needs a draft entry.

Reconcile the docs:

- `TrackState::reach` in track.rs and `#reach` in js/net's track.ts say
  "timestamps need not rise with sequence (a rewind reorders them)". Keep the
  immediate-successor rule and drop the rewind rationale: the bound is the
  immediate servable successor's start, unbounded while that successor is
  unstamped. Tightening `reach` to the first stamped group above, which the
  refusal makes safe, is out of scope; suggest a follow-up if a stall shows
  it matters.
- Delete tests whose premise is a rewind, such as
  `reach_follows_the_immediate_successor_not_a_later_rewind`.
- The container layers already refuse rewinds (moq-mux's
  `container::Producer` returns `TimestampRewind`, and js/hang's `Consumer`
  aborts one). Keep the producers' refusal, which fires before writing and
  also floors every frame, not just a group's first. Delete only consumer
  checks the model now makes unreachable.
- If #5446 lands first, update its note that late-group delivery after a
  rewind is unspecified.

Regression tests in both languages: a timed track refuses a group whose
first frame rewinds, whether appended, created out of order, received, or
(JS) written before `writeGroup`; an equal start is accepted; a resumed
partial group of a valid overlapping pair is accepted; an untimed track
accepts groups in any order.

Public API: write paths can now fail with `TimestampMismatch` on a rewind.
Wire: none if `TimestampMismatch` is reused; the draft's description of
code 0x39 broadens.

## Related

- [#5446](https://github.com/moq-dev/moq/pull/5446) - where the maintainer decided rewinds are never supported
- [JS track handover](/quest/m1/js-group-handover.md) - mirrors Rust's resume contract, which relies on starts never falling
- [Same-epoch importers](/quest/m1/hop-aligned-import.md) - importers already refuse a rewind at the container layer
- [Epoch anchor](/quest/m1/epoch-anchor.md) - a rewind stays fatal to an importer, which restarts under a successor epoch
