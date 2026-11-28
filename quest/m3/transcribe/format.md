# [M] A structured transcript text format

## Goal

Text renditions gain a `transcript` format, in the Rust and JS catalogs
and the hang draft alike. Each group is one utterance, and each frame is a
JSON revision of one of its segments: interim revisions, then a final one,
carrying optional speaker attribution and (on final segments) detected
entities. `@moq/watch` renders a transcript rendition as captions, updating a
segment as its revisions arrive and prefixing speaker labels when present.
Older consumers ignore the format, as they do any unknown one.

## Plan

Decided 2026-11-23 (maintainer, `/quest-run`): ✅ one group per utterance
(rejected: fixed windows on the source clock; following the source video's
groups). ✅ No cap on an utterance's length (rejected: splitting a group near
10 s).

Decided 2026-11-27 (maintainer, `/quest-plan`): ✅ segment-level optional
`speaker` (splitting segments on speaker changes on final results when
diarization is enabled, keeping per-word `speaker` optional) plus optional
`entities` on final segments, with `@moq/watch` displaying speaker labels
when present (rejected: format fields only without `@moq/watch` speaker
rendering; per-word `speaker` only without segment-level `speaker`). Why:
segment-level `speaker` gives players and downstream consumers clean speaker
turns without every consumer re-implementing word-level grouping, while
keeping `entities` structured for programmatic consumers and out of the
caption overlay.

Recommended frame shape, settled in review:

```json
{"id": 41, "final": true, "start": 12.48, "end": 14.02, "lang": "en",
 "speaker": 0,
 "text": "like a big pizza pie",
 "words": [
   {"text": "like", "start": 12.48, "end": 12.61, "confidence": 0.98, "speaker": 0},
   {"text": "a", "start": 12.62, "end": 12.70, "confidence": 0.99, "speaker": 0},
   {"text": "big", "start": 12.71, "end": 12.95, "confidence": 0.97, "speaker": 0},
   {"text": "pizza", "start": 12.96, "end": 13.45, "confidence": 0.98, "speaker": 0},
   {"text": "pie", "start": 13.46, "end": 14.02, "confidence": 0.99, "speaker": 0}
 ],
 "entities": [{"label": "food", "value": "pizza pie", "start_word": 3, "end_word": 5, "confidence": 0.95}]}
```

- `words`, `confidence`, `lang`, `speaker`, and `entities` are optional,
  since a backend or configuration may lack them. `entities` appears only on
  final segments.
- `start_word` is inclusive and `end_word` is exclusive (`[start_word, end_word)`,
  matching Deepgram's convention). Whenever `entities` is present, `words`
  must also be present, and readers refuse any entity whose indices fall out
  of bounds (`start_word >= end_word` or `end_word > words.len()`).
- `speaker` is a 0-based integer local to one continuous recognition session
  or request (not a globally stable identity across reconnects or backfill
  runs). Both `@moq/watch` and text exporters render it as 1-based
  `Speaker {speaker + 1}` (`Speaker 1:` in watch, `<v Speaker 1>` in WebVTT).
- Times sit on the source broadcast's clock, so a transcript lines up with
  the audio it came from. The frame timestamp carries `start`, as for every
  text format, so a relay never parses the payload.
- An utterance runs from speech onset to the silence that ends it, and its
  group from the utterance's start to its end. Silence belongs to no group:
  moq-lite 07 reports the gap between groups as skipped, so a reader knows at
  once that nothing was said ([moq-lite](/doc/concept/moq-lite.md)).
- A frame revises one segment, named by `id`, and a reader keeps the newest
  revision of each. An utterance holds one segment or several, when the
  transcriber commits at a pause inside it, so a frame's size is bounded by a
  segment, and an uncapped utterance's group grows with its text, never with
  the square of it as resending the whole utterance per revision would.

A late joiner gets the current utterance whole: a live join at zero lag
starts at the latest group, which arrives from its first frame, so the reader
replays every segment's revisions and shows the newest at once. A join during
silence starts at the last finished utterance, whose `end` has passed, so
watch shows nothing until the next one opens. An open group is the newest,
which max delay never skips and max age never ages until it finishes, so a
long utterance stays deliverable however long it runs. The track declares the
media max age (30 s), so a segmented exporter still finds the groups behind
the live edge.

Text reaches a player after the speech it transcribes, by the backend's lag
(a translation's more). Watch shows a segment's newest revision as soon as
it arrives, even after its `start`, and keeps one that arrives after its
`end` up for its own duration from arrival, so a player at the live edge
misses no text; a player whose latency target exceeds the lag shows it
aligned with the audio. A group that had already finished when the reader
joined is history rather than late text, so it shows nothing, as above.

Watch highlights words when `words` is present, prefixes a segment's caption
with `Speaker {speaker + 1}:` (a session-local label, not a persistent
identity) when `speaker` is present, and ignores `entities` in the caption
overlay. Its renderer escapes all
markup today (`sanitizeCue` in `js/watch/src/text/renderer.ts`) and lists
`vtt` and `utf8` as renderable (`js/watch/src/text/source.ts`), so add the
format there and build this rendition's DOM from the JSON directly rather
than through media-captions.

Public API: an additive `TextFormat` value (`#[non_exhaustive]` in Rust,
which keeps an unknown string verbatim) and its schema in `@moq/hang`. Wire:
a new catalog `format` string, documented beside `vtt`, `ttml`, and `utf8` in
`drafts/draft-lcurley-moq-hang.md`.
