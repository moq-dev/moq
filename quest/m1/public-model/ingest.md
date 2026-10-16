# [L] Public ingest

## Goal

A custom subscriber writes what it receives into a track through public
moq-net and @moq/net calls only: groups whose header lands before their first
frame, frames streamed across polls with peer-declared sizes, the start its
peer resolves, a received end with groups still in flight, and the session
ending. The built-in lite and IETF subscribers use the same calls.

## Plan

Rules, carve-outs, and the inventory method are in the
[epic](/quest/m1/public-model/README.md). Rerun the inventory first; the
recommendations below are the planning quest's, and the ones marked
**decision** were settled by the maintainer on 2026-10-16 (#5717).

Rust items (`rs/moq-net/src/model/`), as they stood at b4eab0db1:

- **Withheld groups** (`track::Producer::receive_group`, `reveal_group`,
  `group::Producer::is_started`; lite subscriber). **Decision**, reopened
  for the maintainer 2026-10-16 after review:
  - ~~Remove the need: `create_group` withholds every group until its first
    frame or end.~~ Settled earlier, then reopened. It deadlocks a publisher
    that creates a group and waits on `group::Producer::demand()` before
    encoding (the documented "publish only while someone is watching"
    pattern, also exposed by moq-ffi). Group demand counts readers of the
    group, and no reader can reach a withheld group, so the first frame never
    comes.
  - Recommended: `create_group` stays visible at once, so its demand
    works before the first frame. A public `track::Producer::receive_group`
    returns a group that reveals itself on its first frame or its end. There
    is no separate `reveal_group` call to forget, and `is_started` stays
    private. Only a session writing a peer's groups uses it.
  - A public `receive_group` and `reveal_group` pair, as today.
  - Withhold every group, but count a track subscriber that would receive
    it as pre-frame group demand. This keeps one call but couples group
    demand to track cursors.
- **Owned frame writer** (`group::Producer::create_frame_owned`,
  `frame::ProducerOwned`; both subscribers, `coding/reader.rs`). A
  poll-driven wire driver cannot hold the borrowing `frame::Producer<'a>`
  across polls. **Decision**, reopened for the maintainer 2026-10-16 after
  review:
  - Recommended: one owned `frame::Producer` that borrows nothing. The
    runtime `Error::FrameOpen` refusal (`group.rs` already checks it in
    three places) stays the guard. The borrowing `create_frame` and
    `ProducerOwned` both go, and so does the stale "caller's promise" doc on
    `create_frame_owned`.
  - A second, public owned type (AGENTS.md's no-twins rule argues against
    it).
  - ~~The frame writer consumes the `group::Producer` and hands it back when
    the frame finishes, as a compile-time guarantee.~~ Settled earlier,
    then reopened. `group::Producer` is publicly `Clone`, so a clone taken
    before the frame opens can open a second one. The runtime check is the
    real guard either way, as it already is for today's `&mut self`
    borrow.
  - Make the guarantee real: drop public `Clone` from `group::Producer`, or
    add a unique write capability. Not recommended. The crate docs promise
    every producer is cheaply cloneable into another writer, and the model
    keeps clones of its own groups (track slots, abort), so this breaks a
    crate-wide convention to replace a check that already works. Downstream
    holders (moq-ffi, moq-e2ee, moq-transcode) already treat the group as
    exclusive, so the breakage looks small, but no compile probe has
    confirmed it.
- The frame writer's write without a wake (today
  `frame::ProducerOwned::write`) and `notify`: stay private, "tunes the
  built-in sessions" (batches wakes per poll turn).
- **Allocation budget** (`frame::Budget`; both subscribers): remove the
  caller-supplied argument, but keep the aggregate bound. Today one 16 MiB
  budget is shared by every frame a lite or IETF subscriber session
  receives, so a peer that opens many streams declaring maximum-size frames
  gets lazy growth past it, not memory. A per-producer budget would lose
  that, and so would the origin's cache pool, which is unbounded by
  default. Recommended: the budget lives on the origin producer handle a
  session ingests through (the one that already carries its stats and
  identity), with the same default, and is shared by every source, track,
  and group minted from that handle. A frame whose declared size does not
  fit grows with the bytes received, as today. The alternative is no
  up-front allocation at all (always lazy), which needs no budget but pays
  a reallocation per doubling on every large frame. Measure it against the
  ingest benchmarks before choosing it.
- `group::Producer::info` (IETF subscriber): public, mirroring
  `track::Producer::info`.
- **Received end** (`track::Producer::finish_at_pending`, `set_tail_pending`;
  both subscribers). Without it, a custom subscriber over unordered streams
  ends readers at a hole. **Decision**:
  - Public after [Tail arrivals](/quest/m1/tail-arrivals.md), as one call
    that returns a handle the session settles when its tail is in.
  - Keep it private.
  - ✅ Public after Tail arrivals, with the shape left to that quest. Once
    the model records arrivals per track bounded by the subscription's
    grace, it may own the grace timer itself, so a plain `finish_at` on a
    received track could be enough and no settle handle is needed.
- **Session end** (`track::Producer::close`, `abort_session`; both
  subscribers): public. Fold `abort_session` into the public `abort` (abort
  open groups too) unless the implementer finds a reader-visible difference,
  and expose `close` as "end without declaring an end, so a reader resumes
  from another route".
- **Resolved start** (`track::Request::resolving_start`,
  `track::Producer::request_start`; both subscribers): replace with one
  public way to accept a request whose start the peer resolves later. It
  must be set before any reader sees the track, so it lives on
  `track::Request` (for example a pre-accept `request_start(floor)`), with
  the public `start_at` resolving it.
- **Idle copy** (`track::Producer::set_idle`, `set_live`, `is_live`,
  `idle_newest`, `track::IDLE_LINGER`; lite subscriber, `set_idle` also
  IETF). They keep a lingering copy's cache from serving stale groups
  between subscriptions. **Decision**:
  - ✅ Stay private, marked as a known capability gap rather than tuning:
    the staleness they prevent is observable, so "tunes the built-in
    sessions" does not fit. A custom subscriber that ends its track when
    its upstream subscription ends is correct, but cannot keep a lingering
    copy. When designing the resolved start below, check whether idle folds
    into it (re-subscribing a lingering copy is a request whose start the
    peer resolves later); if it does, the gap closes there.
  - Public as an idle handle whose `live(largest)` resumes readers.
- Fetch ingest (`group::Request` fields `state` and `result`,
  `track::TrackState`, `track::FetchOutcome`, `finish_track_at`,
  `poll_landed`): moved to [the deferred quest](/quest/m1/public-model/deferred.md),
  which waits on Model ranges.
- Passthrough, stay private and marked: `group::Producer::wire`,
  `set_wire`, `mark_gap`, `set_end_object`, `group::Wire`, the `object` and
  `datagram` modules, `track::Producer::insert_wire_datagram`,
  `frame::Header`.
- Test-only: `track::Producer::new`, `publisher_priority`, `start_sequence`,
  `group::Producer::is_aborted`, `is_finished`, `group::Info::produce`,
  `track::Info::timed` (see the epic).

JS items (`js/net/src/`): mirror each Rust decision under the same name.

- `hooks.endTrack` (`internal.ts`, IETF subscriber; the JS
  `finish_at_pending`): moved to the deferred quest, which waits on JS
  pending tail.
- `hooks.pendingTrackProducer` (IETF subscriber) reaches the producer behind
  an unaccepted request so objects landing before SUBSCRIBE_OK are kept.
  Replace it with the resolved-start shape above: the request is accepted
  with its start pending, so early data has a public producer to land in.
- `Group.Producer.mirror` (`@internal`, lite subscriber fetch coalescing):
  moved to the deferred quest, which waits on Model ranges and JS track
  handover.
- `internal.ts` `finalSequence` (IETF subscriber) is public with the serving
  quest's `Position::final_sequence`.
- `tail.ts` `Tail` and `TAIL_GRACE_MS`, `consume.ts` `BroadcastCache`: stay
  private. They are session plumbing in both languages (Rust's `tail.rs` and
  consume cache sit outside `model/`).

Tests: each public call gets a mock-time test in its language, and the
built-in subscribers move onto the public calls, so their existing suites
cover the behavior.

## Required

- [Tail arrivals](/quest/m1/tail-arrivals.md) - settles the received-end semantics before they become public

## Related

- [Public model after ranges, handover, and the JS tail](/quest/m1/public-model/deferred.md) - the fetch ingest, `mirror`, and `endTrack` items split out of this quest
