# [M] Design a wrapper seam over moq-net

## Goal

Decide how a wrapper (e2ee, flate, json, or a future one) sits between any
moq-net writer or reader and the network without rewriting each writer per
wrapper. Writers and readers include the moq-mux importers, json, flate, and
applications.

The outcome is a reviewed decision record plus a prototype benchmark, not a
port. It ends by splitting the implementation into follow-up quests sized by
the decision.

Non-goals:
- The runtime seam: [Runtime trait](/quest/m2/runtime-trait.md).
- The transport seam: [Drop web-transport-trait](/quest/m2/drop-web-transport-trait.md).

## Plan

Decided 2026-10-17:

- **Why now.** E2EE is the first wrapper. moq-mux importers write into a
  concrete `moq_net::broadcast::Producer` (about 108 `create_track` call
  sites). flate and json take a concrete `track::Producer`, but each already
  splits a codec layer (`Encoder`/`Decoder`) from track ownership.
- **Interim design (#5822).** The approved e2ee design composes through a
  bridge:
  - Writers fill a local plaintext broadcast.
  - A new add-only `broadcast::Producer::tracks()` watcher lets the e2ee
    bridge carry each track through `moq_e2ee::broadcast` under its physical
    name.
  - This quest compares the trait against that bridge, not against a blank
    slate.
- **Design-only, with a gate.** Implementation splits into follow-up quests
  only after the decision is reviewed.
- **Rust and JS together.** Names mirror across both. The browser e2ee
  publisher has the same concrete-broadcast problem as moq-mux.
- **If a trait wins:**
  - A follow-up quest moves the e2ee mux path onto it and deletes the bridge.
  - `broadcast::Producer::tracks()` stays only if something else uses it.
    Otherwise it is deleted.
- **Placement.** This is Related to the e2ee epic, not Required. E2EE ships on
  the bridge.

Open, for the design:

- Trait versus bridge. Weigh the trait's generic spread or `dyn` cost against
  the bridge's extra in-memory hop.
- Scope: which of the broadcast, track, and group levels it covers, for both
  producers and consumers. Datagrams are in scope too: e2ee wraps them, and
  they have their own append and receive APIs and drop rules.
- Generic or `dyn` in the frame write hot path. No `Send` bounds that native
  needs and wasm can't satisfy (see `rs/AGENTS.md`).
- Group lifecycle hooks. flate's DEFLATE is group-scoped and stateful, and
  e2ee's nonce binds the group and frame sequence, so a per-frame map is not
  enough.
- Whether flate and json become wrappers or stay codecs above a track.
- Demand and preference propagation. moq-mux and the browser publisher gate
  output on `track::Demand`, so a wrapper must not keep encoders active with
  no downstream viewer or drop downstream preference updates. Compare both
  designs on idle and active demand, not only throughput.
- Benchmark trait dispatch against the bridge hop on both sides: the write
  bridge above, and its reverse for readers, where unwrapping once before a
  local broadcast differs from unwrapping per reader. Include a datagram
  workload. Sweep tracks × frames per group, and readers on the read side, so
  a cost that grows with the table shows up as a slope.

## Related

- [E2EE](/quest/m2/e2ee/README.md) - the first wrapper; it ships on the bridge
- [Compressed tracks](/quest/m2/flate.md) - flate's track modes, a candidate wrapper
- [Runtime trait](/quest/m2/runtime-trait.md) - a separate seam, out of scope here
