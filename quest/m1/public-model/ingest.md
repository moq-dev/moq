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
**decision** are the maintainer's (see the PR that added this quest).

Rust items (`rs/moq-net/src/model/`), as they stood at b4eab0db1:

- **Withheld groups** (`track::Producer::receive_group`, `reveal_group`,
  `group::Producer::is_started`; lite subscriber). **Decision**, recommended:
  remove the need. `create_group` withholds every group from readers until
  its first frame lands or it ends, the rule a received group needs, so a
  joiner at the live edge is never handed an older group first. An app
  writes its first frame as it creates a group, so it sees no change. The
  alternative is a public `receive_group` and `reveal_group` pair.
- **Owned frame writer** (`group::Producer::create_frame_owned`,
  `frame::ProducerOwned`; both subscribers, `coding/reader.rs`).
  **Decision**, recommended: replace the borrowing `frame::Producer<'a>`
  with one owned `frame::Producer`, and have the group refuse a second open
  frame with an error instead of the caller's promise. A poll-driven wire
  driver cannot hold the borrow across polls. The alternative is a second,
  public owned type, which AGENTS.md's no-twins rule argues against.
- `frame::ProducerOwned::write` without a wake, and `notify`: stay private,
  "tunes the built-in sessions" (batches wakes per poll turn).
- **Allocation budget** (`frame::Budget`; both subscribers): remove the
  need. A peer-declared frame size is untrusted for every wire, so the model
  bounds up-front allocation itself (from the origin's cache pool, or a
  per-producer budget the model owns) instead of taking a caller's budget.
- `group::Producer::info` (IETF subscriber): public, mirroring
  `track::Producer::info`.
- **Received end** (`track::Producer::finish_at_pending`, `set_tail_pending`;
  both subscribers). **Decision**, recommended: public after
  [Tail arrivals](/quest/m1/tail-arrivals.md) settles the semantics, as one
  call that declares an end received from the wire and returns a handle the
  session settles (or drops) when its tail is in. The paired bool cannot then
  be left set. Without it, a custom subscriber over unordered streams ends
  readers at a hole.
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
  IETF). **Decision**, recommended: stay private, "tunes the built-in
  sessions". They keep a lingering copy's cache from serving stale groups
  between subscriptions; a custom subscriber that ends its track when its
  upstream subscription ends loses only the linger.
- Fetch ingest (`group::Request` fields `state` and `result`,
  `track::TrackState` with `TrackState::abort`, `track::FetchOutcome`,
  `group::Request::finish_track_at`, `track::Consumer::poll_landed`): remove
  the need through [Model ranges](/quest/m1/subscribe-ranges/model.md), which
  replaces per-group requests with ranges; that quest's public range answer
  is the ingest path. Re-inventory once it lands.
- Passthrough, stay private and marked: `group::Producer::wire`,
  `set_wire`, `mark_gap`, `set_end_object`, `group::Wire`, the `object` and
  `datagram` modules, `track::Producer::insert_wire_datagram`,
  `frame::Header`.
- Test-only: `track::Producer::new`, `publisher_priority`, `start_sequence`,
  `group::Producer::is_aborted`, `is_finished`, `group::Info::produce`,
  `track::Info::timed` (see the epic).

JS items (`js/net/src/`): mirror each Rust decision under the same name.

- `hooks.endTrack` (`internal.ts`, IETF subscriber; the JS
  `finish_at_pending`) follows the received-end decision, after
  [JS pending tail](/quest/m1/js-pending-tail.md).
- `hooks.pendingTrackProducer` (IETF subscriber) reaches the producer behind
  an unaccepted request so objects landing before SUBSCRIBE_OK are kept.
  Replace it with the resolved-start shape above: the request is accepted
  with its start pending, so early data has a public producer to land in.
- `Group.Producer.mirror` (`@internal`, lite subscriber fetch coalescing)
  goes with the fetch items: remove the need through Model ranges and
  [JS track handover](/quest/m1/js-group-handover.md), which owns the JS
  fan-out copy.
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

- [Model ranges](/quest/m1/subscribe-ranges/model.md) - replaces the fetch ingest items
- [JS track handover](/quest/m1/js-group-handover.md) - owns `Group.Producer.mirror`
- [JS pending tail](/quest/m1/js-pending-tail.md) - the JS received-end hold
