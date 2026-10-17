# [L] Public serving

## Goal

A custom publisher reads a track for its wire through public moq-net and
@moq/net calls only: where the feed starts, a read cursor its peer can move in
either direction, the track's end in wire terms, and when a group it is still
writing has expired. The built-in lite and IETF publishers use the same calls.

## Plan

Rules, carve-outs, and the inventory method are in the
[epic](/quest/m1/public-model/README.md). Rerun the inventory first; the
recommendations below are the planning quest's, and the ones marked
**decision** were settled by the maintainer on 2026-10-16 (#5717).

Rust items (`rs/moq-net/src/model/`), as they stood at b4eab0db1:

- `track::Subscriber::poll_start` (lite publisher): public. A publisher
  reports the resolved start to its peer (SUBSCRIBE_START, SUBSCRIBE_OK).
- `track::Subscriber::poll_live` (both publishers): remove the need through
  [Live largest location](/quest/m1/largest.md), whose public
  `poll_largest` answers "the largest position once the cache is live".
  Check that it waits out an idle copy the way `poll_live` does.
- **Cursor** (`track::Subscriber::start_at`, `raise_start_to`, `end_at`,
  `subscription::Cap`; both publishers). **Decision**:
  - ✅ One public method assigns the floor and the cap in either direction,
    renamed from `set_groups` to `set_bounds` (JS `setBounds`) on
    `track::Subscriber` and `track::Ordered`, since it sets the reader's
    bounds. Each reader already hands a sequence out once (#5446), so a
    SUBSCRIBE_UPDATE that widens the start re-offers only unread groups.
    `start_at`, `raise_start_to`, `end_at`, and `Cap` go. Precondition:
    lowering the floor re-offers unread cached groups in every mode. Today
    only the resume cursor does; the arrival-order (Plain) cursor walks a
    monotonic arrival index, so it must rescan below its old floor for
    groups it never handed out. Rewrite the doc ("without rewinding read
    progress") to say what lowering does. JS's chaining form,
    `withGroups` on `Track.Subscriber` and `Track.Ordered`, becomes
    `withBounds`.
  - A second public `replace_groups`.
- **Bounds naming** (decided 2026-10-16 in #5717): ✅ the siblings follow the
  rename, so every range setter says bounds. `group::Consumer::set_frames`
  becomes `set_bounds`, and `Subscription::with_groups` becomes
  `with_bounds`, with the JS mirrors (the `Subscription.groups` field
  becomes `bounds`, and any group-level frame bounds setter matches).
- `group::Consumer::start_at`, `end_at` (both publishers): remove the need;
  the public `set_bounds` (today `set_frames`) and `skip_to` cover them.
- `subscription::Position::final_sequence` (lite publisher): public, the
  one-line wire translation of an exclusive end that both languages
  already duplicate. `track::Consumer::final_sequence` (IETF publisher) goes;
  callers read `poll_finished` and translate.
- **Group expiry while writing** (`group::Consumer::poll_expired`,
  `poll_expired_while_pending`; both publishers): public as one
  `poll_expired`. A publisher that already read frames off a group must
  still abandon its stream once the subscription's max delay expires the
  group. Fold the `pending` flag in, or keep it private as tuning if it only
  saves a poll.
- **Cache peeks** (`track::Consumer::peek_latest`, `peek_before`,
  `peek_group`, `is_live`; IETF publisher, for Largest Object and FETCH):
  moved to [the deferred quest](/quest/m1/public-model/deferred.md), which
  waits on Model ranges.
- Passthrough, stay private and marked: `group::Consumer::wire`,
  `end_object`, `last_object`, `first_frame`, `poll_read_object`,
  `poll_read_objects`, `frame::Object`, `frame::Consumer::fields`,
  `track::Subscriber::poll_recv_wire_datagram`.
- Test-only: `track::Consumer::poll_group`, `resume_position`.

JS items (`js/net/src/`):

- `hooks.replaceGroups` (lite publisher): mirror the cursor decision, so
  `Track.Subscriber.setBounds` assigns the floor and the hook goes.
- `hooks.tryRecvGroup` with its `Recv` union, and `hooks.groupChanged`
  (lite publisher). The public `Subscriber.tryRecvGroup()` returns
  `undefined` for both a held boundary and a drained end, so a synchronous
  publisher cannot tell them apart or park. **Decision**:
  - ✅ Public. Return the `Recv` union from the public method and expose
    the change wake (a signal or a promise, matching the package's style).
    The union deliberately has no Rust type: Rust's lite publisher builds
    its private `Recv` from the public `poll_recv_group` (Pending versus
    `Ready(None)`) and `poll_finished`, which a third party composes the
    same way.
  - Mirror Rust exactly: a distinct drained result and a synchronous end
    check instead of the union, plus the wake.
- `hooks.guardGroup`, `hooks.readGroupFrame` with `ReadGroupFrame` (both
  publishers): mirror the Rust group expiry as one public call. Reading a
  frame the publisher `complete()`s holds the group's `closed` until it is
  written; keep that behavior behind the public read rather than a second
  method if it can be.
- `Track.Subscriber.fork` (`@internal`, IETF publisher fill and cache
  reads): moved to the deferred quest with the cache peeks.
- `wire.ts` `Broadcast.subscribe`, `fetchGroup`: remove the need; the public
  `Broadcast.Consumer.track(name)` paths cover them.
- `wire.ts` `Broadcast.largest` and `hooks.cachedLargest`: remove the need
  through Live largest location.
- `wire.ts` `Broadcast.resolveTrackInfo(name, hold)` (both publishers):
  public as an option on the track's info lookup (an `AbortSignal` that keeps
  the track wanted until it fires), mirrored by whatever Rust's `query`
  holds. Check Rust parity first; drop the hold if Rust has no equivalent
  need.
- `internal.ts` `groupBounds`: stays a private helper; a third party passes
  the public `Groups`.
- `priority.ts` `sendOrder`, `Priority`: stay private. They rank streams on
  a `stream.ts` writer, which is wire plumbing in both languages.

Tests: lowering the floor re-offers an unread cached group in both cursor
modes. Each public call gets a mock-time test in its language, and the
built-in publishers move onto the public calls, so their existing suites
cover the behavior.

## Required

- [Narrow the model before it goes public](/quest/m1/public-model/narrow.md) - its visibility rewrite and marker lint guard every model API this child edits
- [Live largest location](/quest/m1/largest.md) - its `poll_largest` replaces `poll_live` and the cached-largest reads

## Related

- [Public model after ranges, handover, and the JS tail](/quest/m1/public-model/deferred.md) - the cache peeks and `fork`, split out of this quest
