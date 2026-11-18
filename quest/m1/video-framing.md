# [M] H.264 and H.265 framing follows the description, and the codec says where parameter sets live

## Goal

Every producer reports the codec string that matches its frames, and every
consumer parses frames by the catalog's `description`, never by the codec
prefix:

- No `description`: frames are Annex B and carry their parameter sets, so
  the codec is `avc3`/`hev1`.
- A `description`: frames are length-prefixed by the record's length size,
  and the record seeds the parameter sets. The codec is `avc1`/`hvc1`
  unless parameter sets also appear in-band, then `avc3`/`hev1`.
- CMAF frames are always length-prefixed.

A Legacy `hev1`/`avc3` track with a record, as `moq publish fmp4` and the
HLS import produce, plays natively (`moq play`, every binding,
moq-transcode) as it does in the browser.

## Plan

Decided 2026-11-17 (maintainer, `/quest-import` of
[moq-dev/moq#9011](https://github.com/moq-dev/moq/issues/9011)):

- ✅ Consumers take framing from `description` (rejected: make the
  fMP4/HLS import rewrite length prefixes to start codes and drop the record,
  so `avc3` always means Annex B; it costs a rewrite per frame and still
  breaks on any other publisher that sends `avc3` with a record). This
  matches the hang draft (`drafts/draft-lcurley-moq-hang.md`, framing by
  `description`), the browser, and every exporter.
- ✅ The browser publisher reports `avc3` for its Annex B H.264 (rejected:
  keep `avc1`). `avc1` with no record contradicts itself, and it makes the
  fMP4 export pull SPS/PPS into a fixed avcC, so a resolution change at a
  keyframe likely fails with `TrackChanged`; reproduce that first. Safari
  rejects `avc3` as an encoder string, so only the catalog's codec changes;
  the watcher already retries `avc3` as `avc1`.
- The maintainer asked that importers detect `avc1` vs `avc3` from the
  bytes, as WebCodecs does: check that an avcC/hvcC input whose stream
  repeats parameter sets in-band reports `avc3`/`hev1`.
- The import format names move in their own breaking quest,
  [video-format-names](/quest/m2/video-format-names.md); this one only fixes
  behavior and wording.

Bugs found while importing (reproduce each first):

1. `rs/moq-video/src/decode/decoder.rs` picks framing from `inline` /
   `in_band` (the `avc3`/`hev1` prefix): a Legacy `hev1` track with an hvcC
   and length-prefixed frames is handed to the backend as Annex B. ffmpeg
   writes `hev1` for HEVC in MP4 by default, so
   `ffmpeg … -c:v libx265 -f mp4 -movflags frag_keyframe+empty_moov | moq publish fmp4`
   then `moq play` fails. Since
   [moq-dev/moq#6250](https://github.com/moq-dev/moq/pull/6250) made the
   Legacy unwrap the default, this is the common fMP4/HLS path. An `hvc1`
   track with no record is refused, where H.264 passes it through as Annex B.
2. `rs/moq-webrtc/src/codec/mod.rs` treats any record as complete: an
   `avc3`/`hev1` record with empty parameter-set lists (what our fMP4 export
   writes) fails with `MissingParamSets` instead of taking them in-band.

Wording to fix (comments and docs that call `avc3`/`hev1` Annex B):
`rs/moq-mux/src/codec/h264/mod.rs`, `h264/import.rs`, `h264/split.rs`,
`h265/import.rs`, `import/track.rs`, `container/source.rs`,
`ts/export.rs`, `flv/export.rs`, `mkv/export.rs`,
`rs/moq-cli/src/subscribe.rs`, `rs/moq-video/src/encode/encoder.rs`,
`rs/moq-webrtc/src/codec/`, `rs/hang/src/catalog/video/h265.rs:14`, and
`doc/concept/hang.md` near line 216 (an absent description is not "what
avc3/hev1 tracks do"). `grep -rniE 'annex.?b' rs js doc` finds the rest.

Public API: none. Wire: none; the browser's catalog codec string changes
from `avc1.*` to `avc3.*`.

Caused by, for the fix PR's `## Caused by`:

- [moq-dev/moq#6250](https://github.com/moq-dev/moq/pull/6250) - made the Legacy unwrap the default, so fMP4/HLS tracks reached the prefix-framed native decoder

## Closes

- [#9011](https://github.com/moq-dev/moq/issues/9011) - avc3/hev1 conflated with Annex B

## Related

- [Import format names](/quest/m2/video-format-names.md) - renames the Annex B input formats away from `avc3`/`hev1`
