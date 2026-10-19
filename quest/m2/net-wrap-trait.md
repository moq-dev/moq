# [M] Design a wrapper seam over moq-net

## Goal

Decide how a wrapper (e2ee, json, or a future one) sits between any moq-net
writer or reader and the network without rewriting each writer per wrapper.
Writers and readers include the moq-mux importers, json, and applications.
Compression is not a wrapper: it moves into moq-net as a track flag
([Compressed tracks](/quest/m1/compress/README.md)).

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
  sites). json takes a concrete `track::Producer`, but already splits a codec
  layer (`Encoder`/`Decoder`) from track ownership.
- **Interim design.** E2EE composes through a bridge: the
  `moq_e2ee::broadcast::{Producer, Consumer}` wrappers (#5876) wrap a concrete
  moq-net broadcast and see every track creation themselves. The e2ee mux path
  (#5822) is on hold to be rebuilt on them. `moq-net` gains no track listing
  for it (#5880 was abandoned). This quest compares the trait against that
  bridge, not against a blank slate.
- **Design-only, with a gate.** Implementation splits into follow-up quests
  only after the decision is reviewed.
- **Rust and JS together.** Names mirror across both. The browser e2ee
  publisher has the same concrete-broadcast problem as moq-mux.
- **If a trait wins,** a follow-up quest moves the e2ee mux path onto it and
  deletes the bridge.
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
- Group lifecycle hooks. e2ee's nonce binds the group and frame sequence, so
  a per-frame map is not enough.
- Terminal propagation. Finish, typed aborts (such as e2ee's authentication
  error), and handle teardown must cross the wrapper at the broadcast, track,
  and group levels, not collapse into `Error::Dropped` or keep a producer
  alive.
- Whether json becomes a wrapper or stays a codec above a track.
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
- [Compressed tracks](/quest/m1/compress/README.md) - compression moves into moq-net as a track flag, so it is not a wrapper
- [Runtime trait](/quest/m2/runtime-trait.md) - a separate seam, out of scope here
