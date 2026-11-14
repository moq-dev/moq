# [M] Decide whether warmup video keeps two decoders

## Goal

Before the cut, the maintainer settles whether a video rendition with a
`warmup` plays through two decoders (min-delay and max-delay) in `@moq/watch`,
`moq_video::decode`, and `moq play`, or through one decoder that joins one
group earlier through the container consumer's existing warmup reach, as audio
does. The docs describe what ships.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
[moq-dev/moq#7555](https://github.com/moq-dev/moq/pull/7555),
[moq-dev/moq#5842](https://github.com/moq-dev/moq/pull/5842), and
[moq-dev/moq#7376](https://github.com/moq-dev/moq/pull/7376) added about
2,800 lines, none on `release`.

- No producer in the stack sets a video `warmup`: the only writers are Opus
  importers and encoders. The [intra-refresh](/quest/m4/intra-refresh/README.md)
  import that would set one moved to m4 on 2026-11-12, for want of a consumer.
- Once `warmup` is nonzero, the max-delay lane never stops
  (`js/watch/src/video/decoder.ts`; `rs/moq-video/src/decode/select.rs` says
  both lanes race in steady state), so a viewer decodes twice for the whole
  session to improve the moments after a join or skip.
- [Dual decoder](/quest/m4/intra-refresh/dual-decoder.md) says Firefox and
  Safari are unverified and HEVC refresh fails in Chromium, while
  `doc/lib/js/watch.md` presents the feature as working.
- The Rust and JS selection policies differ (Rust holds a sticky source,
  JS picks per picture), though the JS docs say Rust does the same.
- `js/hang`'s container consumer and `moq_mux::container::Playout` already
  decode from the group start and present at start plus `warmup`; video opts
  out of passing it.

The two-decoder design was the maintainer's decision on 2026-10-14; the new
evidence is the 2026-11-12 move to m4 and the absence of any producer.

Recommended: (a). Pass `warmup` to the container consumer for video, delete
`Rendition`'s selector and the max-delay lanes in JS, `decode/select.rs` and
`moq play`'s dual path in Rust, keep `insertRecoveryPoint` only if Chromium
needs it for one decoder, trim `doc/lib/js/watch.md`, and rescope the m4
dual-decoder quest to deciding whether a second decoder pays for itself, with
catch-up measurements.

Public API: (a) removes the dual-decoder selection from `@moq/watch` and
`moq_video::decode`, unpublished. Wire: none.

## Waiting

- Maintainer: (a) one decoder now, revisiting two in m4 with measurements (recommended: no producer, and 2x decode for a join-time gain); (b) keep it, documented as Chromium-only and experimental; (c) keep it as is

## Related

- [Transcoder warmup](/quest/m4/intra-refresh/consumer-warmup.md) - its Goal assumes the players' two decoders; reword it if (a) is picked
