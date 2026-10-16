# [M] Plan: a public model that builds any publisher or subscriber

## Goal

Every crate-private or hidden item in moq-net's `model` module, and every
model capability @moq/net keeps from its public API, is either made public or
made unnecessary, so a third party can build its own publisher or subscriber (a
custom transport, relay, or wire) from the public model alone. The output is
a decision per item and the implementation quests that carry them out, not
production code.

Non-goals: changing the wire, or exposing session internals that are not part
of the model (`ietf/`, `lite/`, connection plumbing).

## Plan

Decided 2026-10-11 by the maintainer, in the quest-complete triage of #5283:
track metadata is never private. #5283 makes the exact END_OF_TRACK location
public instead of a crate-private field, and this quest finds the rest.

- Inventory what the built-in sessions reach that a third party cannot. In
  Rust: the roughly 290 `pub(crate)` items under `rs/moq-net/src/model/`,
  plus any `#[doc(hidden)]` model item. In JS: everything `ietf/` and `lite/`
  import that the package does not export, including `src/internal.ts`,
  `src/wire.ts` (`OriginProducer.receive`/`answer`,
  `Broadcast.subscribe`/`requested`), and `@internal`-stripped methods such
  as `Subscriber.fork` and `Group.Producer.mirror`. Group them by what a custom publisher or subscriber would need
  (receiving and revealing groups, finishing at a pending sequence, demand,
  fetch, stats hooks, cache overhead constants).
- For each group, decide one of: make it public with a role-based name
  mirrored in Rust and JS; replace it with a smaller public handle that makes
  misuse unrepresentable; or remove the need. Constants and implementation
  details that only tune the built-in sessions stay private.
- Prove the result by sketching a minimal publisher, subscriber, and
  forwarding relay (route ingestion and answering included) over a toy
  transport against only the public API, in Rust and JS. The inventory, not
  the sketch, defines scope: every inventoried item gets a decision even if
  the sketch never touches it.
- Decided 2026-10-13: the moq-transport passthrough fields (object
  `skip`, object and unknown track properties, END_OF_GROUP status
  objects, datagram object IDs) stay crate-private on purpose. Mark them
  "deliberately private: moq-transport passthrough only". A relay built on
  the public model drops them, as plain moq-lite does.
  Dynamic-group capability and upstream request signaling are part of this
  private relay-only carve-out too.
- Write the implementation quests (likely one Rust and one JS, or one per
  group) under m1, each listing its public API additions for review.

## Related

- [moq-transport passthrough](/quest/m1/ietf-extension/README.md) - the deliberate exception to this rule
- [#5283](https://github.com/moq-dev/moq/pull/5283) - makes the END_OF_TRACK location public, the first item under this rule
