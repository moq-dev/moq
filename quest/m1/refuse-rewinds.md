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

- Rust: `GroupState::stamp` (`rs/moq-net/src/model/group.rs`) sets a
  group's start from its first frame. Every write path calls it:
  `group::Producer::write_frame`, `write_frames`, and `open_frame` behind
  `create_frame` and `create_frame_owned`, which lite and IETF ingress use.
  The track keeps groups by sequence in its `lookup` map
  (`rs/moq-net/src/model/track.rs`).
- JS: `state.timestamp ??= frame.timestamp` in `js/net/src/group.ts` stamps
  the start; the track keeps groups by sequence in its sorted `timeline`
  (`js/net/src/track.ts`).

The rule: when a group's first frame lands, its start must be at or after
the nearest cached, stamped, non-aborted group below it, and at or before
the nearest one above it. Both sides are checked because groups can be
created and received out of sequence order. An equal start is allowed, as
moq-mux's `container::Producer` already allows a keyframe at the previous
group's start. The write fails without writing: every Rust write path
commits the frame (or installs the partial one) before `stamp` today, so
the check runs before any cache, frame, or index change, and the start is
stamped only once it passes. A session receiving such a group aborts the
track with that error and keeps the session. The ingress handlers must
route it there: today Rust's IETF subscriber (`rs/moq-net/src/ietf/subscriber.rs`)
sends `TimestampMismatch` down the generic branch that aborts only the
group, and js/net's (`js/net/src/ietf/subscriber.ts`) closes the track only
for `ProtocolViolation` or `MalformedTrack`. Check the lite subscribers the
same way, and test that the track, not just the group, closes.

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
- Eviction must not drop the bound. An unstamped group whose stamped
  predecessor expires (Rust's `evict_expired_scan`, JS's `#prune`) would
  otherwise have no lower neighbor and accept a rewind. The track keeps the
  sequence and original start of the highest-sequence group it evicted. A
  group above it is checked against both its cached neighbors and that
  watermark, never one in place of the other, since eviction is not a
  sequence prefix (each recently written group is retained on its own). One
  entry, so no history grows with the cache.
- A late or refetched group at or below that watermark (JS `writeGroup`
  accepts long-evicted sequences; Rust takes older groups after `finish`)
  is checked against its surviving cached neighbors plus the watermark as an
  upper bound, but its lower bound may be gone. Open, for the implementer to
  confirm with the maintainer. Recommended: accept it unverified, keep it out
  of every other group's bounds and out of timestamp-driven expiry, so
  backfill below the watermark keeps working (fetched backfill is already
  served by sequence only). Alternative: refuse a late group whose lower
  bound was evicted, which keeps the Goal absolute but refuses legitimate
  backfill of evicted history.

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
  `container::Producer` returns `TimestampRewind`; the Rust and js/hang
  container consumers abort one). Keep all of them: they floor every frame
  against the previous group's start, while the model checks only a group's
  first frame, and a foreign wire publisher bypasses the producer's floor.
- If #5446 lands first, update its note that late-group delivery after a
  rewind is unspecified.

Regression tests in both languages: a timed track refuses a group whose
first frame rewinds, whether appended, created out of order, received
through the streamed-frame path, written after its predecessor was evicted
(including a delayed stamp above the watermark while an older group
survives a non-prefix eviction), or (JS) written before `writeGroup`; an
equal start is accepted; a resumed partial group of a valid overlapping
pair is accepted; a late group below the watermark is refused against a
surviving cached neighbor, follows the chosen rule otherwise, and never
causes a later refetch to be refused; an untimed track accepts groups in
any order.

Public API: write paths can now fail with `TimestampMismatch` on a rewind.
Wire: none if `TimestampMismatch` is reused; the draft's description of
code 0x39 broadens.

## Related

- [#5446](https://github.com/moq-dev/moq/pull/5446) - where the maintainer decided rewinds are never supported
- [JS track handover](/quest/m1/js-group-handover.md) - mirrors Rust's resume contract, which relies on starts never falling
- [Same-epoch importers](/quest/m2/hop-aligned-import.md) - importers already refuse a rewind at the container layer
- [Epoch anchor](/quest/m2/epoch-anchor.md) - a rewind stays fatal to an importer, which restarts under a successor epoch
