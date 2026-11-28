# [M] Add an OpenAI TTS fallback backend for dubbing

## Goal

`moq-transcribe`'s dubbing stage gains an OpenAI TTS backend implementing the
same internal TTS trait as Deepgram Aura-2, covering target languages outside
Aura-2's 7-language set and proving the trait across two distinct TTS
providers.

## Plan

Decided 2026-11-27 (maintainer, `/quest-plan`): ✅ implement OpenAI TTS as a
separate follow-up quest behind [Deepgram Aura-2 dubbing](/quest/m3/transcribe/dub.md)
so the private TTS trait is proven across two providers and languages outside
Aura-2's set are supported without bloating the initial dubbing PR (rejected:
Aura-2 only; OpenAI only; shipping both in one PR).

Guidance:

- Implement the private TTS trait introduced by
  [dub.md](/quest/m3/transcribe/dub.md) against OpenAI's `/v1/audio/speech`
  streaming HTTP endpoint (requesting raw PCM output encoded to 48 kHz Opus
  via `moq-audio`, or direct Opus frames if timing aligns cleanly without
  decode/re-encode overhead).
- Honor the settled low-latency streaming rule: dispatch each committed
  append-only translated clause delta as soon as it arrives and stream the
  HTTP response chunks straight onto the timestamped audio track without
  waiting for the full response body or the whole segment's `final`. Bound
  in-flight clause requests per rendition and preserve strict segment/clause
  order on the output track.
- Allow callers to configure per-language backend/voice selection or fall
  back to OpenAI TTS automatically for languages Aura-2 does not support,
  failing loudly at setup when a requested language lacks credentials or a
  configured voice.
- Reuse the same sequential non-overlapping placement, bounded-lag skip,
  on-demand lifecycle, and session budget as Aura-2.
- Measure clause-to-first-audio latency and cost per hour of dubbed speech,
  and record both beside Aura-2's measurements in `moq-transcribe`'s README.

Public API: additive OpenAI TTS backend/voice options on
`moq_transcribe::Config`. Wire: none.

## Required

- [Dub translated transcripts with Deepgram Aura-2](/quest/m3/transcribe/dub.md) - the dubbing stage and internal TTS trait this implements
