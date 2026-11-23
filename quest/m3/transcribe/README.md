# Live transcription

## Goal

Any hang broadcast can carry a live transcript that players show as captions
and programs read as structure. `moq-transcribe` consumes a broadcast's audio
and publishes a derivative broadcast beside it, as `moq-transcode` does for
video. Its catalog references a text rendition in a new structured transcript
format, produced just in time while subscribed. The language is detected
automatically. A translator later adds one rendition per target language.

Non-goals: speaker diarization, protected media (a platform never reads its
payload), and the captions a source already carries in band (the importer's
work, below).

This README's own work is the end-user docs, once the children land: a
`doc/lib/rs/moq-transcribe.md` page, its row in `doc/lib/rs/index.md`, and the
transcript in the [MoQ for AI](/doc/concept/use-case/ai.md) page.

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
- ✅ Provider backends behind one interface, and a hosted one first: OpenAI
  `gpt-live-transcribe`, chosen for latency (rejected: self-hosted first;
  AWS Transcribe first for its word timing). Word timing is optional, filled
  by backends that return it. A self-hosted backend follows
  ([whisper](/quest/m4/transcribe-local.md)).
- ✅ A fetch of past transcript transcribes the requested span plus a few
  seconds of preceding source audio, and returns only the requested
  segments (rejected: live only, which leaves rewinds without captions).
  Group fetches mirror the source's, as transcode's do.
- ✅ The translator's runtime is decided later, in
  [its quest](/quest/m3/transcribe/translate.md).

Since then, groups are addressed by their start time and never overlap, and a
time start reads the group containing that time
([moq-lite](/doc/concept/moq-lite.md)). A source's audio groups are often one
packet each, so "mirror the source's groups" no longer names a usable
boundary: the [format](/quest/m3/transcribe/format.md) waits on a pick.

Decided 2026-11-23 (maintainer, on
[moq-dev/moq#9443](https://github.com/moq-dev/moq/pull/9443#issuecomment-7091545924)):
captions reach players only as catalog text tracks. The importer strips
CEA-608/708 caption SEI from H.264 and H.265 and publishes the captions as a
text track with `role: "caption"`, so no player parses SEI. A transcript is
one more source on that path: watch's `captions` attribute lists it beside
imported captions, and an exporter treats it like any other text rendition.
