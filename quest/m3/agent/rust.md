# [L] Implement the moq-agent Rust crate

## Goal

A `moq-agent` Rust crate implements the [MoQ agent wire spec](/quest/m3/agent/spec.md)
for both agent servers/workers and native clients over `moq-net`, `hang`,
`moq-audio`, and `moq-json`. An agent publishes one fanout-capable `hang`
broadcast (with faster-than-real-time audio pacing, barge-in track rotation,
structured transcripts, multimodal text/tool/turn events, and an optional
latency timeline window) and consumes one or many participant broadcasts over
`Origin` prefixes, supporting both shared multi-input room sessions and
dedicated per-request worker spawning.

## Plan

Decided 2026-11-27 (maintainer, `/quest-plan`):

- ✅ Generic MoQ-to-MoQ Rust crate with no provider-specific WebSocket
  bridges baked in (rejected: bundling a Deepgram Voice Agent bridge inside
  `moq-dev/moq`).
- ✅ Built on `hang` broadcasts and `catalog.json` extension, multimodal
  (audio, optional video, transcripts, text prompts/replies, tool calls, turn
  events), barge-in audio track rotation, `Origin` prefix room and
  per-session worker modes, and optional standardized latency timeline
  window.

Guidance:

- **Faster-than-real-time audio producer & barge-in rotation:** port the
  lessons proven in the Pipecat MoQ transport into a clean Rust handle on top
  of `moq-audio`:
  - Encode outgoing audio as 48 kHz Opus (avoiding Chrome WebCodecs sample
    rate mismatches) with bounded lookahead pacing (e.g. default 25 s cap,
    staying below typical 30 s player drop ceilings).
  - Re-anchor the encoder epoch after idle turn gaps (e.g. > 250 ms) so
    silence between turns never inflates the player's jitter floor.
  - On barge-in / interruption, when unplayed in-flight audio exceeds the
    rotation floor (e.g. > 300 ms), **drop** (do not cleanly finish) the
    current audio track producer so it leaves the catalog immediately and
    discards future-dated frames, rewind publish PTS using the exact rule in
    [spec.md](/quest/m3/agent/spec.md), and open the successor rendition
    `<track>-<n>`. Capture track generation before any pacing sleep so a
    stale chunk never lands on the successor.
- **Event track & timeline window:** publish and consume the bounded
  compressed `moq-json` window conversation track (text prompts/replies,
  tool calls and results, turn state, barge-in, session end) with
  checkpointing, replay of only unacknowledged prompts and pending tool
  calls/results (`seq`/`epoch` deduplication across reconnects), and sender
  backpressure at the per-peer bound, as defined in
  [spec.md](/quest/m3/agent/spec.md). Provide an optional typed latency
  timeline producer/consumer (`moq-json` window) bounded to the latest
  exchange.
- **Multi-input & worker spawning over `Origin` prefixes:**
  - **Room / multi-input mode:** subscribe to participant broadcasts matching
    an `Origin` prefix and yield typed per-participant input streams (audio,
    optional video, text prompts, tool results, client turn signals) tagged
    by broadcast path/identity, while publishing one fanout agent broadcast.
  - **Dedicated worker mode:** watch an `Origin` prefix for newly announced
    request broadcasts (or lazy subscriptions to session paths) and hand each
    to a caller-owned session task, leaving admission, auth, and quotas to
    the embedding service.
- **API & tests:** follow `AGENTS.md` and `REVIEW.md` (single lifecycle enum
  per session/turn state, no callback parameters, return handles/streams,
  bound all per-peer state). Write deterministic unit tests on a manual/paused
  clock covering lookahead pacing, barge-in track rotation and PTS rewind,
  idle re-anchoring, reconnect deduplication, multi-input attribution,
  fanout to multiple subscribers, and optional timeline window pruning.

Public API: a new `moq-agent` workspace crate. Wire: implements
[spec.md](/quest/m3/agent/spec.md).

## Required

- [MoQ agent wire specification and hang catalog extension](/quest/m3/agent/spec.md) - the wire format, catalog extension, and schemas this implements
