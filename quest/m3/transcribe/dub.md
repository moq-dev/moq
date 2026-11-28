# [L] Dub translated transcripts with Deepgram Aura-2

## Goal

`moq-transcribe` can be configured to dub chosen target languages into
voice-only, language-tagged audio renditions in the derivative catalog,
produced on demand only while subscribed. Each dubbed rendition streams
append-only translated clause deltas into Deepgram Aura-2 TTS
(`POST https://api.deepgram.com/v1/speak`) behind a private TTS backend
trait, encodes the synthesized PCM to Opus via `moq-audio`, and publishes
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
- ✅ Stream append-only translated clause deltas into TTS as they arrive
  (dispatching each committed clause immediately and any remaining tail on
  `final`) to keep end-to-end dubbing latency as low as possible (rejected:
  waiting for each translated segment's `final` before calling TTS).
- ✅ Queue dubbed clips sequentially without overlap
  (`max(source_start, prev_clip_end)`), shed/skip segments only if lag
  exceeds a bounded cap derived from the advertised `AudioConfig::delay`
  playout budget, and measure real lag drift before considering
  time-stretching (rejected: time-stretching with `moq-audio::stretch` up
  front; truncating an active clip when the next source segment starts).
- ✅ Publish voice-only synthesized audio renditions (no ducked source audio
  mixed underneath), letting players switch cleanly via `audio-lang` or mix
  client-side if desired (rejected: ducking and mixing original source audio
  inside `moq-transcribe`).

Guidance:

- **Streaming synthesis (Deepgram Aura-2):** send each committed clause
  delta to Aura-2's streaming HTTP endpoint
  (`POST https://api.deepgram.com/v1/speak` with the configured Aura-2 voice
  model such as `aura-2-thalia-en` or `aura-2-elara-de`, `encoding=linear16`,
  and `mip_opt_out=true`) using bounded in-flight concurrency and strictly
  ordered output, encoding incoming `linear16` PCM chunks to 48 kHz Opus
  through `moq-audio` as soon as they arrive. Using streaming HTTP per
  committed clause avoids Aura-2 WebSocket's 20 flushes per 60 seconds limit
  (which ordinary speech committing a clause every ~2 s exhausts within a
  minute, leaving short clauses buffered in silence). Fail loudly at config
  setup if a requested dubbing language has no configured voice or is
  unsupported by the selected backend.
- **Clause-delta streaming:** read the translator's append-only revisions in
  process ([translate](/quest/m3/transcribe/translate.md)). Dispatch each
  newly committed clause delta immediately as its own streaming synthesis
  request (never waiting until segment `final`), and on `final` synthesize
  any remaining tail text. Write synthesized PCM chunks onto the audio track
  immediately as they return in clause order so players render them by
  timestamp with minimal latency.
- **Timeline, playout delay, overlap, and backlog:** stamp each clip's first
  audio frame at `max(source_segment_start, previous_clip_end)` on the source
  broadcast's clock, advancing monotonically across consecutive synthesized
  frames within that clip. Gaps between non-contiguous clips are skipped time
  (do not publish silent PCM frames and do not re-anchor off the source
  clock): explicitly finish the previous audio group at the clip's actual end
  timestamp before opening the next clip's group at its start timestamp, so
  moq-lite declares the silent span between clips as skipped rather than
  stretching the previous group across the gap. Because STT, translation, and
  TTS add seconds of pipeline lag behind live source audio, advertise that
  lag via `AudioConfig::delay` in the derivative catalog
  (`rs/hang/src/catalog/audio/mod.rs`), which `@moq/watch` already reads
  (`js/watch/src/audio/decoder.ts`) in `auto` delay mode to raise
  `sync.out.delay` when the dubbed rendition is selected so dubbed frames
  play in sync with video instead of being skipped as late (callers pinning a
  fixed delay must set it to at least the dubbing lag). Set the dubbing
  backlog skip threshold from that playout latency budget (a bounded margin
  past `delay`, capped by the track's `max_age`, rather than the full 30 s
  `max_age`), dropping queued text and skipping forward to the newest
  translated segment if the backend falls behind that budget. Never overlap
  two clips on one track.
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
