# [M] A structured transcript text format

## Goal

Text renditions gain a `transcript` format, in the Rust and JS catalogs
alike. Each frame is a JSON segment, and the segment's group holds its
revisions: interim frames, then a final one. `@moq/watch` renders a transcript
rendition as captions, updating a segment as its revisions arrive. The HLS
and fMP4 exporters write final segments as WebVTT. Older consumers ignore the
format, as they do any unknown one.

## Plan

Recommended segment shape, settled in review:

```json
{"id": 41, "final": false, "start": 12.48, "end": 14.02, "lang": "en",
 "text": "like a big pizza pie",
 "words": [{"text": "like", "start": 12.48, "end": 12.61, "confidence": 0.98}]}
```

- `words`, `confidence`, and `lang` are optional, since a backend may lack
  them (OpenAI's live model returns no word timing or confidence).
- Times sit on the source broadcast's clock, so a transcript lines up with
  the audio it came from. The frame timestamp carries `start`, as for every
  text format, so a relay never parses the payload.
- One group per segment, so a reader that wants only the newest revision
  takes the group's last frame, and a late joiner sees whole segments.

Watch highlights words when `words` is present. Its renderer escapes all
markup today (`sanitizeCue` in `js/watch/src/text/renderer.ts`), so build
this rendition's DOM from the JSON directly rather than through
media-captions.

Public API: an additive `TextFormat` value (`#[non_exhaustive]` in Rust)
and its schema in `@moq/hang`. Wire: a new catalog `format` string.
