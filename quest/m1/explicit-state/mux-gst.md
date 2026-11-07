# [M] moq-mux and moq-gst lifecycles

## Goal

The lifecycles of moq-mux and moq-gst export and ingest are explicit state:
export sources, TS and fMP4 export tracks and programs, the HLS segment
window, the container consumer's startup, and the GStreamer sink and source
pads. Each phase's data lives in its variant, and no `.expect()` or flag
ordering stands in for a phase.

## Plan

The shared decisions are in the [epic](/quest/m1/explicit-state/README.md).

No confirmed bug is folded into this child. Reproduce these two first:

- **fMP4 export** (`rs/moq-mux/src/container/fmp4/export.rs`): a track that
  ends before it is ever described, while the catalog stays live, may keep
  `init_ready()` false forever.
- **TS export track** (`rs/moq-mux/src/container/ts/export.rs`):
  - Withdrawing a track keeps its parked frames, so it can become both
    `withdrawn` and `listed`.
  - `finished` carries five meanings, and the "gone" predicate ANDs eight
    conditions.

Instances to redesign:

- **TS export `Program`**: `catalog`, `catalog_ended`, `cataloged`,
  `retired`, `left`, `psi`, and others. `resume` hand-resets six fields.
- **`ExportSource`** (`container/source.rs`): `media`, `priority_control`,
  `instance`, `live`, and `after` belong inside the `SourceState` variants.
- **Container `Consumer`** (`container/consumer.rs`): the startup phase and
  the `track`/`track_error` reader state. The JS twin is
  `js/hang/src/container/consumer.ts`, whose `#checkMaxDelay` moves `#active`
  without `#recordPresented`. Change both together.
- **HLS** (`rs/moq-hls`):
  - The segment window keeps `ended`, `cut`, and `failed`, and `push` after
    `end()` isn't refused.
  - Retiring a `Claim` takes two locks.
  - The importer's `TrackState` generation fields.
- **Catalog publish gate** (`catalog/producer.rs`): `Reservations` writes its
  predicate twice, in two shapes.
- **TS `Follower.refused`** belongs inside `Serving`. The TS `Passthrough`
  start and clock, and the `GroupBuffer` content kind, also become explicit.
- **moq-gst sink pads** (`rs/moq-gst/src/sink/request_pad.rs`, `pad.rs`,
  `imp.rs`):
  - The `ended` bool means something different from `Status::Ended`.
  - `releasing` is cleared after removal.
  - The element's `drained`/`lingering`/`eos_delivered` and the pad's
    `reserved`/`track`/`catalog`/`caps`/`failed` encode phases.
- **moq-gst source** `Entry`'s `reader`/`draining`.
- **Others**:
  - The RTMP `ServerSession`: the connected app name belongs inside
    `SessionState::Connected`.
  - The moq-webrtc ICE `Agent`: its failure isn't sticky.
  - The p2p channel handout.
  - The jitter `Track`.
