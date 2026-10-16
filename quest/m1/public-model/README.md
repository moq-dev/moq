# A public model that builds any publisher or subscriber

## Goal

A third party builds its own publisher, subscriber, or forwarding relay (a
custom transport, relay, or wire) from the public moq-net and @moq/net model
alone. Every model item a built-in session reaches is public, made
unnecessary, or marked deliberately private with its reason.

Non-goals: changing the wire, or exposing session internals that are not part
of the model (`ietf/`, `lite/`, connection plumbing, wire codecs).

## Plan

Planned 2026-10-16 from the inventory below; the per-item decisions live in
the children's Plans, and the maintainer settled the open ones in #5717.

Rules every child follows:

- Track metadata is never private (decided 2026-10-11 in the #5283 triage).
- Each item is made public under a role name mirrored in Rust and JS,
  replaced by a smaller public handle that makes misuse unrepresentable, or
  made unnecessary. Each child lists its public API additions and removals
  in its PR for review.
- Deliberately private, marked in a comment on the item so the next
  inventory skips it:
  - "moq-transport passthrough only" (decided 2026-10-13; subgroups were
    dropped from it in #5704): object IDs and `skip`, object and unknown track properties, END_OF_GROUP status
    objects, datagram object IDs, dynamic groups, and upstream request
    signaling. Owned by [moq-transport passthrough](/quest/m1/ietf-extension/README.md).
    A relay built on the public model drops them, as plain moq-lite does.
  - "tunes the built-in sessions": constants and wake or allocation batching
    that change cost, never behavior a third party can observe.
  - "capability gap": a built-in behavior a third party can observe but
    not reproduce yet, kept private by a recorded decision that names what
    would close it (the idle copy in [ingest](/quest/m1/public-model/ingest.md)).
- Items replaced by another quest's new API wait for that quest instead of
  growing a public shape it deletes: fetch and per-group requests
  ([Model ranges](/quest/m1/subscribe-ranges/model.md)), largest and
  liveness ([Live largest location](/quest/m1/largest.md)), announce
  interest ([Rust lazy discovery](/quest/m1/lazy-discovery/rust.md)), and
  the received tail ([Tail arrivals](/quest/m1/tail-arrivals.md)).

Inventory method, to rerun before each child starts (counts drift). Rust:
replace every `pub(crate)` under `rs/moq-net/src/model/` with
`pub(in crate::model)`, then run `cargo check -p moq-net --features fuzz`
and restore each item the compiler names until it builds; repeat with
`--tests` for test-only reach. JS: list what `src/{ietf,lite,connection}/`
import from `internal.ts`, `wire.ts`, and modules `index.ts` does not
export, plus `@internal` members (`stripInternal` hides them from the
published types).

Inventory on `main` at b4eab0db1:

- Rust has 347 `pub(crate)` items in `model/` and no `#[doc(hidden)]` model
  item. 106 are reached by session code, 16 only by session tests, and 225
  only inside `model/`.
- The 106 split into [ingest](/quest/m1/public-model/ingest.md),
  [serving](/quest/m1/public-model/serve.md),
  [routes and answering](/quest/m1/public-model/answer.md), and the
  passthrough carve-out (listed in each child where it shares a type).
- The 16 test-only items (`group::Info::produce`, `group::Producer::is_aborted`
  and `is_finished`, `track::Info::timed`, `track::Producer::new`,
  `publisher_priority` and `start_sequence`, `track::Consumer::poll_group`
  and `resume_position`, `FetchOutcome::rejected`,
  `origin::Producer::announce` with `AnnounceProducer` and `ProduceTest`,
  `origin::Consumer::get_broadcast`) stay crate-private test helpers: a third
  party tests through the public API it uses.
- The 225 model-internal items stay private.
- JS publishes only `src/index.ts`, with `stripInternal` on. Sessions reach
  past it through `internal.ts` (helpers and 22 `hooks`, 7 of them called
  by sessions: `endTrack`, `pendingTrackProducer`, `tryRecvGroup`,
  `groupChanged`, `replaceGroups`, `guardGroup`, `readGroupFrame`), the
  `wire.ts` views (`Broadcast`, `OriginProducer`, `OriginConsumer`,
  `Established`), `@internal` members (`Track.Subscriber.fork`,
  `Group.Producer.mirror`), and modules the index does not export
  (`hop.ts` values, `announced.ts` `Producer`, `consume.ts`, `tail.ts`,
  `priority.ts`). Model-internal hooks (sequence namespaces, timestamps,
  arrival, eviction, announcer stamping) stay private. Each session-reached
  item is assigned in a child, except `wire.ts` `Established` (`consume`,
  `goaway`). It is connection plumbing outside the model, a session handle
  the connection layer uses, and stays private like the other excluded
  plumbing.

Work owned by this README, after the children land:

- Rerun the inventory and confirm every `pub(crate)` left in `model/` is a
  marked passthrough, tuning, capability-gap, or test-only item, so the
  "pending" marks from [Narrow the model](/quest/m1/public-model/narrow.md)
  are all gone.
- Prove the result with a toy transport that uses only the public API: an
  integration test under `rs/moq-net/tests/` (outside the crate, so the
  compiler enforces it) and a JS test importing only the package entrypoint.
  Each runs a minimal publisher, subscriber, and forwarding relay with route
  ingestion and answering, on mock time, in CI.

## Required

- [Narrow the model](/quest/m1/public-model/narrow.md) - a new model item a session reaches fails to compile until it is made public or marked, while the other children are in flight
- [Public ingest](/quest/m1/public-model/ingest.md) - a custom subscriber writes received groups, frames, starts, and ends into a track through public calls
- [Public serving](/quest/m1/public-model/serve.md) - a custom publisher reads a track's start, cursor, and group expiry for its wire through public calls
- [Public routes and answering](/quest/m1/public-model/answer.md) - a custom relay ingests routes, answers broadcast and track requests on demand, and advertises without echoing a peer's routes back
- [Public model after ranges, handover, and the JS tail](/quest/m1/public-model/deferred.md) - fetch and track-info answering, the cache peeks, JS `fork`/`mirror`, and JS `endTrack`, once the quests that reshape them land

## Related

- [moq-transport passthrough](/quest/m1/ietf-extension/README.md) - the deliberate exception to this rule
- [#5283](https://github.com/moq-dev/moq/pull/5283) - made the END_OF_TRACK location public, the first item under this rule
