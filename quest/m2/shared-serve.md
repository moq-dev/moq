# [L] Shared group serving and ingest

## Goal

In `rs/moq-net`, the moq-lite and moq-transport sessions share one poll
machine for serving a track's groups and one for ingesting them. Serving
covers priority rank, stream credit, give-up, and deadlines. Ingesting
covers reading a group stream into the model. Each wire plugs in its own
group header and object or frame encoding through a small trait. Behavior,
wire, and public API are unchanged. The duplicated copies are deleted, such
as `poll_given_up`, which is identical in `lite/publisher.rs` and
`ietf/publisher.rs`.

Non-goals: request lifecycles, announce handling, FETCH, and JS. JS is
[its own quest](/quest/m2/shared-serve-js.md).

## Plan

Decided with the maintainer on 2026-10-28 (`/quest-plan`, "cut epoch over
moq-transport", asking how hard combining the moq-lite and moq-transport code
would be).

Why: a full shared session core is XL, 4 to 6 quests. The IETF stack is
mostly `async fn` lifecycles, while lite is hand-written `kio::Task` poll
machines. The wires also differ in Request IDs and aliases, FETCH, and
objects with subgroups and properties. Only about 5% of the two publishers'
lines match. The data plane is the one place both are already poll machines,
which makes it the cheapest shared slice. It would remove about 0.8 to 1k
lines per stack on the publisher side alone. It does not make MoQ Epoch
cheaper, since epoch semantics already live in the model.

Decision prompts:

- Plan a shared core: not now; ✅ in m2; in m1.
- Scope: Rust first, then JS, publisher only; ✅ also the subscriber side;
  Rust and JS together; Rust only.
- Rank: ✅ after Perf and the transport benches, so they show no regression;
  with the moq-transport work; end of m2.

Implementation guidance:

- **Serving.** Lite's `TrackRun`, `Rank`, and `GroupServe`
  (`rs/moq-net/src/lite/publisher.rs`), and the IETF `TrackServe`,
  `Ranking`, `Ranked`, and `GroupServe` (`rs/moq-net/src/ietf/publisher.rs`).
- **Ingest.** Lite's `GroupRecv` and `FrameIngest`
  (`rs/moq-net/src/lite/subscriber.rs`), and the IETF `GroupIngest`
  (`rs/moq-net/src/ietf/subscriber.rs`). Both have an `IngestPhase`.
- Which existing machine becomes the base is decided while implementing and
  recorded in the PR. The trait carries only what differs on the wire:
  stream headers, object or frame encoding, subgroups, properties, and
  Location deltas. Datagrams stay per wire unless sharing them turns out
  simpler.

Tests: the existing publisher, subscriber, and integration suites pass
unchanged across every version. Session benches in `rs/moq-net/benches`
show no regression against `main`. Run `just check` and
`just test interop --all`.

Public API: none. Wire: none.

## Related

- [Shared group serving and ingest in JS](/quest/m2/shared-serve-js.md) - the JS mirror
- [Perf](/quest/m2/perf/README.md) - hot-path work in the same machines
