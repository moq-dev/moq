# [M] A structured transcript text format

## Goal

Text renditions gain a `transcript` format, in the Rust and JS catalogs
and the hang draft alike. Each frame is a JSON revision of a transcript
group's segments: interim frames, then a final one. `@moq/watch` renders a
transcript rendition as captions, updating a segment as its revisions
arrive. Older consumers ignore the format, as they do any unknown one.

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
- A reader that wants only the newest revision takes the group's last frame,
  and a late joiner sees whole groups.

Which span a group covers waits on the pick below. The plan said one group
per segment, but groups are now addressed by their start time and never
overlap, and a time start reads the group containing that time
([moq-lite](/doc/concept/moq-lite.md)). Segment boundaries come from the
transcriber's turn detection, which depends on where it started listening, so
two fetches over the same past span can cut different groups.

Watch highlights words when `words` is present. Its renderer escapes all
markup today (`sanitizeCue` in `js/watch/src/text/renderer.ts`) and lists
`vtt` and `utf8` as renderable (`js/watch/src/text/source.ts`), so add the
format there and build this rendition's DOM from the JSON directly rather
than through media-captions.

Public API: an additive `TextFormat` value (`#[non_exhaustive]` in Rust,
which keeps an unknown string verbatim) and its schema in `@moq/hang`. Wire:
a new catalog `format` string, documented beside `vtt`, `ttml`, and `utf8` in
`drafts/draft-lcurley-moq-hang.md`.

## Waiting

- Maintainer: pick what span a transcript group covers. Options: fixed
  windows on the source clock (for example every two seconds, at multiples of
  the window), each group's frames revising the segments that start in it,
  so every fetch cuts the same groups and audio-only broadcasts work;
  mirror the source video's groups, as `moq-transcode` does, which lines
  captions up with join points but leaves audio-only broadcasts without a
  boundary; or keep one group per segment and answer fetches only from groups
  a live run already produced, which drops the settled fetch of past spans.
  Recommendation: fixed windows. An agent cannot take it, since it reopens a
  settled plan.
