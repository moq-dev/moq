# [M] MoQ agent wire specification and hang catalog extension

## Goal

Define the wire specification for MoQ-to-MoQ conversational agents in
`drafts/` (with any media-specific additions placed in
`drafts/draft-lcurley-moq-hang.md` so generic `@moq/watch` players support
them), along with the shared catalog extension types, event/timeline schemas,
and cross-language JSON parity fixtures for Rust and TypeScript.

## Plan

Decided 2026-11-27 (maintainer, `/quest-plan`):

- ✅ Layer directly on `hang` broadcasts and `catalog.json` via a new
  `Catalog<E>` root extension section for non-media agent state/tracks,
  brainstorming the broadcast suffix convention (for example `.agent` or
  `.agent.hang`) during spec design, while keeping media-specific behavior
  in `drafts/draft-lcurley-moq-hang.md` (rejected: standalone non-hang
  broadcast format; uncataloged fixed track names).
- ✅ Multimodal schema: standard `hang` audio and optional video renditions,
  `transcript` text renditions ([format.md](/quest/m3/transcribe/format.md)),
  bounded compressed `moq-json` conversation/event track for text prompts,
  agent text replies, tool calls/results, turn state (`listening`,
  `thinking`, `speaking`), and session/barge-in signals, plus an optional
  standardized `moq-json` latency timeline window (rejected: voice +
  transcripts only; omitting a standardized timeline window).
- ✅ Barge-in audio track rotation: an agent writing faster-than-real-time
  TTS retires its outgoing audio track on barge-in (dropping it from the
  catalog so stale future-dated audio is discarded), rewinds publish PTS by
  the unplayed in-flight lead (`max(0, pacing_clock - now)`, where
  `pacing_clock` is the presentation deadline of the last written chunk, so
  the successor starts at the media time playing now), and publishes a
  successor rendition `<name>-<n>`; idle turn gaps re-anchor the encoder
  epoch so silence does not inflate jitter (rejected: reusing one monotonic
  audio track with one group per turn).
- ✅ Pure MoQ `Origin` prefix/announce conventions for both multi-input room
  sessions and per-request dedicated worker spawning, plus unlimited fanout
  of the agent's broadcast (rejected: 1:1 pairs only; built-in job-queue
  broker protocol).

Guidance:

- **Catalog extension & broadcast naming:** keep `hang::Catalog` clean by
  defining the non-media agent section as a catalog extension (`agent` or
  similar) pointing to the agent's JSON tracks (events track, optional
  timeline window) and declaring role/capabilities. Decide the broadcast
  suffix (such as `.agent` beside `.hang`) consistently with existing
  conventions so relays, rooms, and players distinguish agent broadcasts at a
  glance without breaking generic `hang` consumers.
- **Media-specific pieces in `hang`:** document faster-than-real-time audio
  buffering (`lookahead`), utterance-boundary re-anchoring, and catalog-driven
  audio track retirement on interruption in `drafts/draft-lcurley-moq-hang.md`
  where generic players (`@moq/watch`) already implement them.
- **Conversation event track & bounded replay:** use a compressed `moq-json`
  `window` track advertised in the catalog (replacing uncataloged RTVI
  `transcript.json.z`, and avoiding an unbounded single-group `stream` whose
  prefix cannot be recovered after long sessions). Specify record types for
  user text prompts, agent text output (streaming deltas + final), tool call
  requests and client tool results, turn state transitions,
  interruption/barge-in notifications, and clean session-end markers. Define
  a periodic checkpoint for rejoining readers and retain only unacknowledged
  prompts, active turn state, and pending tool calls/results for replay
  (with `seq`/`epoch` deduplication across reconnects), applying sender
  backpressure when unacknowledged history reaches its per-peer bound.
- **Latency timeline window:** specify the optional `moq-json` window schema
  (modeled on Pipecat's `voice.json` v2): versioned records for raw frame
  arrivals, VAD and turn transitions, LLM request/first-text/end, TTS
  request/first-audio/end, published audio spans, word timings, and track
  retirements, bounded to the latest exchange per side so late joiners get
  the active turn without unbounded growth.
- **Topology & prefixes:** specify how an agent subscribes to multiple input
  broadcasts under a shared `Origin` prefix (room mode, attributing audio
  and text inputs by broadcast path) while publishing its own fanout
  broadcast, and how a dedicated worker watches an `Origin` prefix to spawn
  an isolated agent per announced request broadcast or lazy subscription.
- **Parity fixtures:** add shared JSON parity fixtures exercised by both
  Rust and TypeScript tests so neither side drifts.

Public API: new wire draft(s) and shared schema/catalog extension types in
Rust and TypeScript. Wire: additive `hang` catalog extension and agent JSON
track schemas (older `hang` consumers ignore unknown extension sections).

## Required

- [A structured transcript text format](/quest/m3/transcribe/format.md) - the transcript text rendition format agent broadcasts reuse for live speech
