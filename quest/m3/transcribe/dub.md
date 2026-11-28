# [L] Dub translated transcripts with Deepgram Aura-2

## Goal

`moq-transcribe` can be configured to dub chosen target languages into
voice-only, language-tagged audio renditions in the derivative catalog,
produced on demand only while subscribed. Each dubbed rendition streams
append-only translated clause deltas into Deepgram Aura-2 TTS over WebSocket
(`wss://api.deepgram.com/v1/speak`) behind a private TTS backend trait,
encodes the synthesized PCM to Opus via `moq-audio`, and publishes
non-overlapping audio aligned to the source segment timeline.

Non-goals: voice cloning or speaker-matched voices, mixing ducked original
audio underneath, and protected media.

## Plan

Decided 2026-11-27 (maintainer, `/quest-plan`):

- ✅ Add an on-demand dubbing TTS stage to `moq-transcribe`, building
  Deepgram Aura-2 first behind a private TTS backend trait and proving the
  trait with an [OpenAI TTS fallback](/quest/m3/transcribe/dub-openai.md) as
  its follow-up quest (rejected: Deepgram Aura-2 only; OpenAI TTS only;
  combining both providers into one PR; dropping dubbing).
- ✅ Stream append-only translated clause deltas into TTS as they arrive,
  flushing on `final`, to keep end-to-end dubbing latency as low as possible
  (rejected: waiting for each translated segment's `final` before calling
  TTS).
- ✅ Queue dubbed clips sequentially without overlap
  (`max(source_start, prev_clip_end)`), shed/skip segments only if lag
  exceeds a bounded cap (such as the track's max age), and measure real lag
  drift before considering time-stretching (rejected: time-stretching with
  `moq-audio::stretch` up front; truncating an active clip when the next
  source segment starts).
- ✅ Publish voice-only synthesized audio renditions (silence between clips),
  letting players switch cleanly via `audio-lang` or mix client-side if
  desired (rejected: ducking and mixing original source audio inside
  `moq-transcribe`).

Guidance:

- **Streaming synthesis (Deepgram Aura-2):** connect to
  `wss://api.deepgram.com/v1/speak` with the configured Aura-2 voice model
  (which carries the target language, such as `aura-2-thalia-en` or
  `aura-2-elara-de`), `encoding=linear16`, and `mip_opt_out=true`. Aura-2's
  WebSocket does not stream Opus natively, so encode incoming `linear16` PCM
  to 48 kHz Opus through `moq-audio`. Fail loudly at config setup if a
  requested dubbing language has no configured voice or is unsupported by the
  selected backend.
- **Clause-delta streaming:** read the translator's append-only revisions in
  process ([translate](/quest/m3/transcribe/translate.md)). Send each newly
  appended clause delta as a `Speak` message and send `Flush` when the
  segment's `final` arrives, writing synthesized PCM chunks onto the audio
  track immediately as they return so players render them by timestamp with
  minimal latency.
- **Timeline, overlap, and backlog:** stamp each segment's first audio frame
  at `max(source_segment_start, previous_clip_end)` on the source broadcast's
  clock, advancing monotonically across consecutive synthesized frames and
  re-anchoring across silent gaps so idle gaps do not inflate the player's
  jitter estimate. Never overlap two segments on one track. When dubbing lag
  past the live source exceeds the track's max age (30 s), drop queued text
  and skip forward to the newest translated segment so a slow backend causes
  a bounded gap rather than unbounded memory growth.
- **Catalog & on-demand lifecycle:** advertise each dubbed rendition in the
  derivative catalog's `audio` section with its BCP-47 `lang`
  ([audio-lang](/quest/m3/audio-lang.md)) and a clear label distinguishing
  dubbed audio from the source's original audio. Start the TTS session (and
  the upstream translation it reads) only while the dubbed rendition has a
  subscriber, stop once the last subscriber leaves, and count active TTS
  sessions toward `moq-transcribe`'s session budget.
- **Private TTS trait:** design the internal TTS interface around streaming
  text deltas in and yielding timed PCM chunks out (no callback parameters,
  matching `AGENTS.md`), keeping it private until
  [the OpenAI TTS backend](/quest/m3/transcribe/dub-openai.md) implements it.
- **Measurements:** measure latency from translated clause delta to first
  published Opus frame, end-to-end speech-to-dubbed-audio latency, and how
  much lag accumulates on a multi-minute clip when translated speech runs
  longer than the source. Record those numbers in `moq-transcribe`'s README
  so any later time-stretching decision is grounded in data.

Public API: additive dubbing config on `moq_transcribe::Config` and dubbed
audio renditions in the derivative catalog. Wire: none beyond
[audio-lang](/quest/m3/audio-lang.md).

## Required

- [Translate a transcript into chosen languages](/quest/m3/transcribe/translate.md) - the append-only translated clause deltas this voices
- [Audio renditions carry a language](/quest/m3/audio-lang.md) - BCP-47 `lang` on dubbed `AudioConfig` renditions so players select languages
