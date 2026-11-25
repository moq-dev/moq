# [M] A structured transcript text format

## Goal

Text renditions gain a `transcript` format, in the Rust and JS catalogs
and the hang draft alike. Each group is one utterance, and each frame is a
JSON revision of one of its segments: interim revisions, then a final one.
`@moq/watch` renders a transcript rendition as captions, updating a segment
as its revisions arrive. Older consumers ignore the format, as they do any
unknown one.

## Plan

Decided 2026-11-23 (maintainer, `/quest-run`): ✅ one group per utterance
(rejected: fixed windows on the source clock; following the source video's
groups). ✅ No cap on an utterance's length (rejected: splitting a group near
10 s).

Recommended frame shape, settled in review:

```json
{"id": 41, "final": false, "start": 12.48, "end": 14.02, "lang": "en",
 "text": "like a big pizza pie",
 "words": [{"text": "like", "start": 12.48, "end": 12.61, "confidence": 0.98}]}
```

- `words`, `confidence`, and `lang` are optional, since a backend may lack
  them.
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

Watch highlights words when `words` is present. Its renderer escapes all
markup today (`sanitizeCue` in `js/watch/src/text/renderer.ts`) and lists
`vtt` and `utf8` as renderable (`js/watch/src/text/source.ts`), so add the
format there and build this rendition's DOM from the JSON directly rather
than through media-captions.

Public API: an additive `TextFormat` value (`#[non_exhaustive]` in Rust,
which keeps an unknown string verbatim) and its schema in `@moq/hang`. Wire:
a new catalog `format` string, documented beside `vtt`, `ttml`, and `utf8` in
`drafts/draft-lcurley-moq-hang.md`.
