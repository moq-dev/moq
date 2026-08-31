# [M] hang: publisher-reported stats track (catalog stats section)

## Goal

Implement and verify the behavior tracked in [#2734](https://github.com/moq-dev/moq/issues/2734)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

### hang: publisher-reported stats track (catalog `stats` section)

Design context: moq-dev/moq.pro#992. The network layer (per-subscription send backlog, skip counters - see moq-dev/moq#2733) covers congestion; this issue is the *media* half: health signals only the publisher can know, published by the publisher into its own broadcast.

#### Spec

A new root catalog section following the pattern the draft already blesses ("a `chat` section should include the name of a chat track, not individual chat messages"; `Catalog` is `#[non_exhaustive]` / `z.looseObject` with a tested extension round-trip):

```json
{ "stats": { "track": "stats.json" } }
```

The track: one group per snapshot, JSON, cumulative counters (so a mid-stream joiner gets totals), ~1 update/second, optionally a deflate + merge-patch `.z` sibling like moq-stats. Section in `drafts/draft-lcurley-moq-hang.md` or a small companion draft.

Proposed vocabulary (cumulative unless noted; per rendition where it matters):

- `sourceFrames`, `sourceStalls`, `sourceStallMs` - capture layer
- `encodedFrames`, `keyframes`, `encodeQueueDepth` (gauge) - encoder layer
- `targetBitrate`, `encodedBitrate` (gauges) - what ABR is doing
- `queuedBytes` (gauge), `sendRate`, `rtt` - uplink, from the local transport + PROBE
- `active` (gauge) - whether encoding is currently demand-driven

#### Implementation notes

- `js/publish`: `Video.Encoder.out.stats` already folds `{frames, bytes, keyframes}` - extend in place. The capture reader loop (`Capture.#run`) is the chokepoint for source-stall watchdogging; `Epoch` already detects a stuck source clock but the detection is neither counted nor exported.
- **Gate everything on demand** (`Encoder.out.active` / `Demand`): encoding is demand-driven and the camera is released when demand drops, so an ungated stall metric reports every idle broadcast as stalled.
- Rust: same hooks in moq-video's encode/capture, so moq-cli and gateway-style ingest (RTMP/SRT) can publish the same track from ingest-side observations.
- Consumers that don't know the section ignore it (tested); consumers that do can subscribe or not - the catalog only names the track.

#### Related spec fix rolled in

`Timeline.wall` is the sanctioned wall-clock anchor and is generally unset today. Publishers SHOULD set it: it turns every viewer's latency from "relative to my join" into a real end-to-end figure, and the JS publisher already rebases capture timestamps onto its own wall clock (`Epoch`), so setting it is nearly free.

## Closes

- [#2734](https://github.com/moq-dev/moq/issues/2734) - close this issue when the quest finishes
