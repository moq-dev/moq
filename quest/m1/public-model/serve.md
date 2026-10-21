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
  the public `poll_largest` (#5725). It now waits out an idle copy too
  (#5924), but it follows the source's reported largest, which can name a
  frame the cache does not hold yet, so the publishers still answer with the
  buffered edge. A public live signal is
  [its own quest](/quest/m1/track-live.md). Decide here whether wire answers move to it, and
  with them `latest_sequence` on `track::{Producer,Consumer,Subscriber}`,
  the private snapshot that replaced the public `latest()` (#5725).
- **Cursor** (`track::Subscriber::start_at`, `raise_start_to`, `end_at`,
  `subscription::Cap`; both publishers). **Decision**:
  - ✅ One public consuming builder assigns the floor and the cap in either
    direction: `with_range(self, range) -> Self` on `track::Subscriber` and
    `track::Ordered` (JS `withRange`), replacing `set_groups` and the
    chaining `withGroups`. The maintainer decided "range" on 2026-10-19,
    replacing the 2026-10-18 `limit` and the earlier `set_bounds`. Each
    reader already hands a sequence out once (#5446), so a SUBSCRIBE_UPDATE
    that widens the start re-offers only unread groups.
    `start_at`, `raise_start_to`, `end_at`, and `Cap` go. Precondition:
    lowering the floor re-offers unread cached groups in every mode. Today
    only the resume cursor does; the arrival-order (Plain) cursor walks a
    monotonic arrival index, so it must rescan below its old floor for
    groups it never handed out. Rewrite the doc ("without rewinding read
    progress") to say what lowering does.
  - Rebinding: a consuming builder fits readers bounded once at creation,
    but these callers move a live reader's bounds through `&mut` today, so
    the rename needs a way to rebind (take and replace the reader, or a
    crate-private setter the builder wraps):
    - `rs/moq-c/src/consume.rs` `RawReader::apply` re-applies the range on
      every subscription update, counting a relative start from the track
      `Consumer`'s unbounded largest. `set_groups` only raises the floor, so an
      update cannot lower a floor an earlier one put above the live edge
      (Codex on #5924). Making the floor assign fixes it; land that with a
      moq-c test where a later relative update delivers the next group.
    - The lite and IETF publishers move the cursor on SUBSCRIBE_UPDATE
      (`start_at`, `end_at`; JS `hooks.replaceGroups` and the IETF
      publisher's `setGroups({ start })`).
    - `js/json` snapshot and `js/flate` `latest()` raise the floor of the same
      reader on each read; `js/room` chat sets it after `largest()` resolves.
      The snapshot consumers (`js/json/src/snapshot/consumer.ts` `#read`,
      `js/flate/src/snapshot/consumer.ts` `latest()`) call
      `setGroups({ start: { included: latest.group } })`, whose omitted end
      removes the caller's cap. The rebinding must keep that cap; land it with
      a regression test where a capped snapshot reader stays below its cap.
      A datagram also advances `largest()` past every group, stranding the
      read. The Rust consumers instead drain the groups the reader already
      has and keep the newest (#5677), which needs no floor at all.
    - `rs/moq-hls` export narrows a fetched group with `set_frames(..end)`.
    - Readers that set a floor once right after `poll_largest` (moq-audio,
      moq-video, moq-cli play, moq-json window, moq-mux) fit the builder.
  - ✅ The range floor counts as a local bound (maintainer, 2026-10-19):
    `largest()` reports within both the floor and the cap, so a reader whose
    floor sits above the live edge reports nothing until a group reaches it
    (#5924).
  - ✅ `largest()` reports only upstream-confirmed state (maintainer,
    2026-10-19): the source's reported largest and what its live feed
    delivered since, never a cache-derived stand-in. It waits while the copy
    is idle, though cached groups are still served, and past the cap it
    reports nothing rather than the newest cached group below it (#5924).
  - A second public `replace_groups`.
- **Bounds naming** (decided 2026-10-16 in #5717, changed to "range" by the
  maintainer on 2026-10-19): ✅ the siblings follow:
  `Subscription::with_range` replaces `with_groups` as the single-range form
  of `with_ranges`, and `group::Consumer::set_frames` becomes the consuming
  `with_range`, with the JS mirrors.
- `group::Consumer::start_at`, `end_at` (both publishers): remove the need;
  the public `with_range` (today `set_frames`) and `skip_to` cover them.
- `Subscription::{start, end, set_bounds, with_start, with_end}` (both
  publishers, IETF subscriber, from #5732): the covering span a wire without
  ranges carries. Derive it from the public `ranges` instead, or make it
  public.
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
  `Track.Subscriber.withRange` assigns the floor and the hook goes.
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
- `wire.ts` `Broadcast.largest`, `hooks.cachedLargest`,
  `hooks.bufferedLargest`, and `hooks.latestSequence`: remove the need through the public `largest()`
  stream (#5725), under the same caveat as `poll_live`.
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

## Related

- [Public model after ranges, handover, and the JS tail](/quest/m1/public-model/deferred.md) - the cache peeks and `fork`, split out of this quest
