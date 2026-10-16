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
  in review and settled 2026-10-16:
  - ~~`create_group` withholds every group until its first frame or end.~~
    It deadlocks a publisher that creates a group and waits on
    `group::Producer::demand()` before encoding, since no reader can reach
    a withheld group to demand it.
  - A public `receive_group` that reveals itself on its first frame or end.
  - A public `receive_group` and `reveal_group` pair, as today.
  - ✅ Drop withholding. Hiding a group from readers was never what fixed
    #4914's rejoin bug: the model's judgments must not count a frameless
    group as content. Derive that from each group's own state (first frame
    opened, or ended) for every group, wherever the model asks it: the live
    edge, successor and staleness, the rejoin wait (`shows`), `newest` and
    `largest`, and `set_idle`. Every group joins arrival order when created,
    so readers and group demand reach it before its first frame.
    `create_group` is the only call; `receive_group`, `reveal_group`, and
    `is_started` go. The group wakes its track once, on its first frame or
    end, as `reveal` does today. Verified on a scratch branch at dd95d5804
    for received groups: with arrival order immediate and the judgments
    gated, the whole moq-net suite passes except the two tests that assert
    the old design. Consequences:
    - `largest()` stops reporting a frameless newest group as (N, 0) and
      reports the last frame that exists. This touches
      [Live largest location](/quest/m1/largest.md); align with it there.
    - Regression guard: `rejoin_waits_for_the_answers_first_frame`
      (`rs/moq-net/tests/rejoin.rs`, fails when the judgments count a
      frameless group) and
      `revealing_a_presented_group_wakes_a_read_it_expires` stay;
      `a_withheld_group_holds_the_end` and
      `a_withheld_group_that_ends_empty_releases_the_end` are rewritten to
      "a frameless group does not count as content" rather than "a reader
      cannot see it".
    - Unverified: app-created groups on the derived path, and the JS
      mirror. Check both here before landing.
- **Owned frame writer** (`group::Producer::create_frame_owned`,
  `frame::ProducerOwned`; both subscribers, `coding/reader.rs`). A
  poll-driven wire driver cannot hold the borrowing `frame::Producer<'a>`
  across polls. **Decision**, reopened in review and settled 2026-10-16:
  - One owned `frame::Producer` guarded only by the runtime
    `Error::FrameOpen` refusal.
  - A second, public owned type (AGENTS.md's no-twins rule argues against
    it).
  - ✅ Make one open frame per group a compile-time guarantee. Drop public
    `Clone` from `group::Producer`; the model and sessions keep a
    crate-private duplicate. The chunked writer is owned:
    `group::Producer::create_frame(self, info) -> Result<frame::Producer>`
    aborts the group with the error on failure (an oversized frame, a
    timestamp mismatch, or a closed group are bugs or protocol errors),
    `frame::Producer::finish(self) -> Result<group::Producer>` hands it
    back, and `frame.group()` gives read access (demand, info, closed)
    while the frame is open. `write_frame(&mut self)` stays for one-shot
    frames. The borrowing `create_frame` and `ProducerOwned` go, with the
    stale "caller's promise" doc; `FrameOpen` stays only as a backstop for
    crate-internal duplicates. `track::Producer` keeps `Clone`: several
    writers creating groups is legitimate, and `claim_sequence` already
    refuses a duplicate sequence.
    - Compile probe at dd95d5804: 26 `group::Producer` clone sites, 16
      model-internal and 7 in the sessions (kept by the crate-private
      duplicate), and 3 outside the crate (2 in
      `rs/moq-net/benches/track.rs`, 1 moq-ffi test), all trivial.
    - The sessions clone to abort a group held elsewhere and to watch
      demand or closure. If a third-party session needs to abort without
      owning the writer, add a non-writing abort handle rather than
      `Clone`.
    - Pitfall: `group::Producer` derefs to the `Copy` `group::Info`, so a
      downstream `producer.clone()` silently copies the `Info` and fails
      later with a confusing "no method on `Info`" error. Say so in the
      changelog, or drop the `Deref` for an `info()` getter.
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
  `idle_newest`, `track::IDLE_LINGER`, `poll_past`; lite subscriber,
  `set_idle`, `set_live`, and `poll_past` also IETF). They keep a lingering copy's cache from serving stale groups
  between subscriptions. **Decision**:
  - ✅ Stay private, marked as a known capability gap rather than tuning:
    the staleness they prevent is observable, so "tunes the built-in
    sessions" does not fit. A custom subscriber that ends its track when
    its upstream subscription ends is correct, but cannot keep a lingering
    copy. When designing the resolved start below, check whether idle folds
    into it (re-subscribing a lingering copy is a request whose start the
    peer resolves later); if it does, the gap closes there.
  - Public as an idle handle whose `live(largest)` resumes readers.
  - `track::Producer::poll_past` (IETF resume: a group past the answer's
    Largest marks the copy live) belongs to this item and shares its fate:
    private while the idle copy is, public only with it. Fix it either way:
    it reads `max_sequence`, which a frameless group header advances, so it
    must wait for a later group that has started (a frame landed or it
    ended), the gating rule above.
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

- [Narrow the model before it goes public](/quest/m1/public-model/narrow.md) - its visibility rewrite and marker lint guard every model API this child edits
- [Tail arrivals](/quest/m1/tail-arrivals.md) - settles the received-end semantics before they become public

## Related

- [Public model after ranges, handover, and the JS tail](/quest/m1/public-model/deferred.md) - the fetch ingest, `mirror`, and `endTrack` items split out of this quest
