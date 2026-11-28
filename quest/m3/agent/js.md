# [L] Implement the @moq/agent TypeScript package

## Goal

A `@moq/agent` TypeScript package (`js/agent`) implements the
[MoQ agent wire spec](/quest/m3/agent/spec.md) on top of `@moq/publish`,
`@moq/watch`, `@moq/hang`, `@moq/json`, and `@moq/signals`. Browser and Node
apps can connect to any `moq-agent` broadcast for voice, text, or multimodal
conversations (publishing mic/camera/text/tool results and playing agent
audio/video/transcripts/text/tool calls with automatic barge-in buffer reset
and optional latency timeline telemetry), join multi-participant agent rooms,
or subscribe as read-only fanout spectators.

## Plan

Decided 2026-11-27 (maintainer, `/quest-plan`):

- ✅ Ship `@moq/agent` in TypeScript alongside Rust `moq-agent` under the
  same wire spec so web clients speak native MoQ directly instead of
  maintaining ad-hoc RTVI glue (rejected: Rust-only first).
- ✅ Multimodal hang broadcasts, catalog-driven barge-in track rotation +
  `reset()`, multi-participant `Origin` prefix rooms and dedicated sessions,
  fanout spectator support, and optional standardized latency timeline
  window.

Guidance:

- **Client session API:** compose `@moq/publish` (mic audio, optional
  camera/screen video, user text prompts, tool results, optional client
  turn/barge-in signals) and `@moq/watch` (auto-configuring `lookahead` for
  faster-than-real-time TTS bursts and calling `reset()` immediately on
  barge-in events or retired audio track removal so buffered audio stops
  without draining). Expose reactive `@moq/signals` state for turn phase
  (`listening`/`thinking`/`speaking`), structured transcripts, streaming text
  messages, incoming tool calls, and the optional latency timeline window.
- **Fanout & multi-participant support:** support read-only spectator
  consumption of an agent broadcast (zero publish side needed), 1:1
  dedicated sessions (announce request broadcast under a prefix and subscribe
  to the spawned agent broadcast), and multi-participant room prefixes
  alongside `@moq/room`.
- **Reconnect resilience:** replay the user's lossless `moq-json` event log
  across reconnects with `seq`/`epoch` deduplication matching
  [spec.md](/quest/m3/agent/spec.md), and handle agent broadcast restarts
  cleanly without resurrecting stale turns.
- **Verification:** add unit tests on a mocked clock for barge-in `reset()`,
  reconnect dedupe, tool-call round-trips, and timeline window updates, plus
  a cross-language interop test pairing Rust `moq-agent` with JS `@moq/agent`
  (audio turn pacing, barge-in track rotation, text/tool events, and timeline
  window parity).

Public API: a new `@moq/agent` workspace package (`js/agent`). Wire:
implements [spec.md](/quest/m3/agent/spec.md).

## Required

- [MoQ agent wire specification and hang catalog extension](/quest/m3/agent/spec.md) - the wire format and catalog extension this implements
- [Implement the moq-agent Rust crate](/quest/m3/agent/rust.md) - the Rust peer for cross-language interop tests
