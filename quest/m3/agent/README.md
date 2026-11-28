# MoQ conversational agents

## Goal

Conversational AI agents (voice, text, and multimodal) and their clients
speak native MoQ to each other in Rust (`moq-agent`) and TypeScript
(`@moq/agent`), layered directly on `hang` broadcasts and `catalog.json`.
Agents take advantage of MoQ's native fanout (one agent broadcast consumed by
any number of subscribers), multi-input sessions (one agent consuming
multiple participant broadcasts under an `Origin` prefix), dedicated
per-session spawning over `Origin` announce prefixes, faster-than-real-time
audio with instant barge-in track rotation, structured transcripts, tool
calls, and an optional standardized latency timeline window.

Non-goals: vendor-specific WebSocket bridges (such as a Deepgram Voice Agent
bridge that must run on an edge close to the provider), fleet hosting,
admission/billing, and protected media.

This README's own work is the end-user docs, once the children land:
`doc/lib/rs/moq-agent.md`, `doc/lib/js/agent.md`, their index rows, and
updating [MoQ for AI](/doc/concept/use-case/ai.md) to show `moq-agent` and
`@moq/agent` in practice.

## Plan

Decided 2026-11-27 (maintainer, `/quest-plan`):

- ✅ Plan a generic MoQ-to-MoQ `moq-agent` wire spec, Rust crate
  (`moq-agent`), and TypeScript package (`@moq/agent`) in `moq-dev/moq`,
  while keeping vendor-specific WebSocket edge bridges (such as Deepgram
  Voice Agent) out of this repository (rejected: shipping a Deepgram-only
  Voice Agent bridge crate in `moq-dev/moq`; shipping an optional Deepgram
  backend in `moq-dev/moq`; dropping agent work from `moq-dev/moq`). Why: a
  WebSocket bridge has to run on a CDN edge close to the provider rather than
  over the public internet, whereas an open MoQ-to-MoQ agent protocol lets
  browsers, native apps, edge workers, and providers speak native MoQ
  directly.
- ✅ Build directly on `hang` broadcasts and `catalog.json`, adding a new
  catalog extension section for non-media agent metadata/tracks (turn state,
  text/tool streams, capabilities) and choosing a clear broadcast path
  extension (such as `.agent`, brainstormed in
  [spec.md](/quest/m3/agent/spec.md)), while putting anything media-specific
  into the `hang` spec itself so generic `@moq/watch` players support it
  (rejected: a standalone non-hang broadcast format or uncataloged fixed
  track names like `transcript.json.z`).
- ✅ Support multimodal `hang` broadcasts: audio, optional video, structured
  transcripts, text prompts and replies, tool calls/results, and turn events
  (rejected: voice + transcripts + barge-in only).
- ✅ Rotate the agent's audio track on barge-in (dropping the retired audio
  producer from the catalog when unplayed audio is in flight, rewinding PTS
  as specified in [spec.md](/quest/m3/agent/spec.md), and opening a successor
  `<track>-<n>`, while re-anchoring across idle turn gaps), matching the
  proven Pipecat MoQ transport design (rejected: reusing one persistent audio
  track with one group per turn). Why: faster-than-real-time TTS writes
  future-dated timestamps seconds ahead, so reusing a monotonic track breaks
  timestamps and STT interruption alignment.
- ✅ Take advantage of MoQ fanout and multi-participant `Origin` prefixes:
  one agent broadcast can be watched by many subscribers, one agent can
  consume multiple input broadcasts under a shared prefix (room mode), and
  pure MoQ `Origin` prefix/announce conventions support spawning a dedicated
  agent session per announced request or lazy subscription (rejected: 1:1
  paired broadcasts only; building a job-queue broker/claim protocol inside
  `moq-agent`).
- ✅ Include an optional standardized latency timeline window (a `moq-json`
  window modeled on `voice.json`: frame arrivals, VAD/turn transitions,
  LLM/TTS milestones, word spans, and track retirements) so demos and
  benchmarks share one observability format (rejected: leaving diagnostic
  timelines entirely to callers).
