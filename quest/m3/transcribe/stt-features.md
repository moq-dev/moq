# [M] Opt-in Deepgram STT features in moq-transcribe

## Goal

`moq-transcribe` exposes opt-in Deepgram Nova-3 live transcription features:
speaker diarization, PII redaction, entity detection, and keyterm prompting.
Callers enable any subset on the crate's config, and published transcript
frames carry speaker-split segments with optional `speaker` IDs and (on final
segments) detected `entities`, while redaction masks sensitive spans in place
and keyterms boost domain vocabulary. Backends that do not support a
requested option fail loudly at setup rather than silently ignoring it.

Non-goals: pre-recorded-only Audio Intelligence (summarization, topic
detection, sentiment analysis, intent recognition), and protected media.

## Plan

Decided 2026-11-27 (maintainer, `/quest-plan`):

- ✅ Extend `moq-transcribe` with opt-in Deepgram Nova-3 diarization,
  redaction, entity detection, and keyterm prompting, reversing the epic's
  2026-11-17 non-goal on speaker diarization (rejected: dropping the
  extension; bundling pre-recorded Audio Intelligence here).
- ✅ Split segments on speaker changes when diarization is enabled so each
  segment carries one optional `speaker` ID (keeping per-word `speaker`
  optional), carry optional `entities` on final segments, and render speaker
  prefixes in `@moq/watch` (defined in
  [the format](/quest/m3/transcribe/format.md); rejected: format fields only
  without `@moq/watch` rendering; per-word `speaker` only).

Guidance:

- Map each opt-in flag onto Deepgram `/v1/listen` query parameters while
  preserving `mip_opt_out=true`:
  - Diarization: `diarize_model=latest` (or `diarize=true`), splitting a
    Deepgram result at word-level speaker transitions so each emitted segment
    has a single `speaker` ID and clean start/end timestamps on the source
    clock. When a result splits into multiple segments, adjust each segment's
    word-index references (such as `entities` `start_word`/`end_word`) to
    match that segment's `words` slice, or keep entity indices valid per
    segment.
  - Redaction: repeated `redact=<kind>` parameters (for example `pii`,
    `pci`, `phi`, `numbers`, or granular Deepgram redaction labels); Deepgram
    masks the returned `transcript` and `words` directly.
  - Entity detection: `detect_entities=true`, mapping Deepgram's final
    `entities` array (`label`, `value`, `start_word`, `end_word`,
    `confidence`) onto the final segment's `entities` field.
  - Keyterm prompting: repeated `keyterm=<term>` query parameters (never
    comma-joined). Validate non-empty keyterms and any provider limits at
    config setup rather than failing silently at runtime.
- Keep `mip_opt_out=true` on every streaming and gap/pre-recorded request,
  and ensure gap runs pass the same opt-in flags so rewound spans match live
  output.
- When [the local whisper backend](/quest/m4/transcribe-local.md) joins
  later, unsupported flags return a typed setup error rather than warning and
  continuing.
- Test speaker-change segment splitting, entity index remapping, and option
  validation with deterministic fixture responses on a mocked clock.

Public API: additive optional fields on `moq_transcribe::Config`. Wire: none
beyond [the transcript format](/quest/m3/transcribe/format.md).

## Required

- [moq-transcribe with a Deepgram backend](/quest/m3/transcribe/crate.md) - the crate and Deepgram streaming client this extends
