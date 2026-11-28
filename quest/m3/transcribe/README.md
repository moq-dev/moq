# Live transcription and captions

## Goal

Any hang broadcast can carry a live transcript that players show as captions
and programs read as structure. `moq-transcribe` consumes a broadcast's audio
and publishes a derivative broadcast beside it, as `moq-transcode` does for
video. Its catalog references a text rendition in a new structured transcript
format, produced just in time while subscribed, with opt-in speaker
diarization, PII redaction, entity detection, and keyterm prompting. The
language is detected automatically, from the text when the backend reports
none. A translator adds one rendition per target language, translated from the
transcript's text, and an optional dubbing stage voices each translated
rendition into a language-tagged audio rendition on demand.

Captions a source carries in band reach players the same way: the importer
takes CEA-608/708 out of the video and publishes them as text tracks, and the
exporters write every text rendition, transcripts included, in the forms each
container carries.

Non-goals: pre-recorded-only Audio Intelligence (summarization, topic
detection, sentiment analysis, intent recognition), voice cloning or
speaker-matched dubbing voices, and protected media (a platform never reads
its payload).

This README's own work is the end-user docs, once the children land: a
`doc/lib/rs/moq-transcribe.md` page (covering transcription, opt-in STT
features, translation, and dubbing), its row in `doc/lib/rs/index.md`, the
transcript in the [MoQ for AI](/doc/concept/use-case/ai.md) page, and
imported and exported captions on the import and export docs.

## Plan

Decided 2026-11-17 (maintainer, `/quest-plan`):

- ✅ A crate modeled on `moq-transcode`: consume a source, publish a
  derivative whose catalog references the source's renditions plus the
  transcript, so the transcriber never proxies what it does not produce
  (rejected: adding a CLI command now; transcriber only, without the
  translator).
- ✅ A structured transcript format replaces WebVTT as the output (rejected:
  WebVTT; WebVTT plus a JSON track). Why: WebVTT has no interim or final
  state, confidence, or machine-readable words, which AI consumers want;
  exporters still write WebVTT. See [the format](/quest/m3/transcribe/format.md).
- ✅ Provider backends behind one interface, and a hosted one first
  (rejected: self-hosted first). Word timing is optional, filled by backends
  that return it. A self-hosted backend follows
  ([whisper](/quest/m4/transcribe-local.md)). The hosted backend is
  Deepgram, decided 2026-11-24 below.
- ✅ A fetch of past transcript transcribes the requested span plus a few
  seconds of preceding source audio, and returns only the requested
  segments (rejected: live only, which leaves rewinds without captions).

Decided 2026-11-23 (maintainer, on
[moq-dev/moq#9443](https://github.com/moq-dev/moq/pull/9443#issuecomment-7091545924)):
✅ captions reach players only as catalog text tracks (rejected: players parse
SEI on decode). The importer strips CEA-608/708 caption SEI from H.264 and
H.265 and publishes the captions as a text track with `role: "caption"`
([caption import](/quest/m3/transcribe/caption-import.md)). A transcript is
one more source on that path: watch's `captions` attribute lists it beside
imported captions, and an exporter treats it like any other text rendition.

Decided 2026-11-23 (maintainer, `/quest-run`):

- ✅ One transcript group per utterance, with no cap on its length (rejected:
  fixed windows on the source clock; following the source video's groups;
  splitting near 10 s). This replaces "group fetches mirror the source's", as
  groups are now addressed by their start time and a source's audio groups
  are often one packet each. [The format](/quest/m3/transcribe/format.md)
  says how a late joiner gets the open utterance, and
  [the crate](/quest/m3/transcribe/crate.md) how a fetch answers a past span.
- ✅ `role: "caption"` for the source-language transcript (rejected: a new
  `transcript` role).
- ✅ Exporters write every text format: WebVTT, SRT, and CEA-608/708 where the
  container carries it (rejected: WebVTT only), split between
  [text tracks](/quest/m3/transcribe/export.md) and
  [captions in the video](/quest/m3/transcribe/caption-export.md).
- ✅ Caption import is planned here (rejected: leaving it to
  [moq-dev/moq#9443](https://github.com/moq-dev/moq/pull/9443)'s author).
- ✅ The translator starts on a hosted OpenAI model: `gpt-realtime-2` in a
  text-only session translating the transcript's segments
  ([translate](/quest/m3/transcribe/translate.md)), and the self-hosted
  transcriber on whisper.cpp through `whisper-rs`
  ([transcribe-local](/quest/m4/transcribe-local.md)).

Decided 2026-11-24 (maintainer, `/quest-plan`): ✅ Deepgram Nova-3 is the
only hosted transcription backend (rejected: OpenAI `gpt-live-transcribe`
first, which 2026-11-17 picked for latency; OpenAI first with Deepgram
second; AWS Transcribe). Why: Deepgram streams interim and final results,
utterance ends, word timing, and each word's language, which the format
wants and OpenAI's live model lacks, at about a third of its price per
minute. The translator stays on OpenAI, since it translates text.

Decided 2026-11-27 (maintainer, `/quest-plan`):

- ✅ Extend `moq-transcribe` and the structured transcript format with opt-in
  Deepgram Nova-3 features: speaker diarization (reversing the 2026-11-17
  diarization non-goal), PII redaction, entity detection, and keyterm
  prompting ([STT features](/quest/m3/transcribe/stt-features.md); rejected:
  dropping the extension). Pre-recorded-only Audio Intelligence
  (summarization, topics, sentiment, intents) stays out of scope.
- ✅ Add an on-demand dubbing TTS stage on top of translated transcripts,
  voicing append-only translated clause deltas into voice-only,
  language-tagged audio renditions with sequential non-overlapping placement
  and bounded-lag skip: Deepgram Aura-2 first behind a private TTS trait
  ([dubbing](/quest/m3/transcribe/dub.md)), followed by an OpenAI TTS
  fallback ([OpenAI TTS](/quest/m3/transcribe/dub-openai.md)) to prove the
  trait across streaming WebSocket and HTTP backends and cover languages
  outside Aura-2's set (rejected: Aura-2 only; OpenAI only; ducked original
  audio mix; time-stretching before measuring lag; truncating active clips;
  waiting for final segments instead of streaming clause deltas).
