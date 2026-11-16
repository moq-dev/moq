# [M] Decide whether a second decoder pays for itself

## Goal

Measure what a second decoder would buy a viewer of an intra-refresh rendition
(catalog `warmup` set), and either add it to `@moq/watch`, `moq_video::decode`,
and `moq play` or delete this quest. Once
[catalog warmup](/quest/m4/intra-refresh/catalog-warmup.md) restores it, each
player decodes through one decoder that joins one group earlier and hides the
warmup after each join or skip, so a skip freezes the picture for one sweep.

## Plan

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)):
one decoder ships, and two are revisited here with measurements.
[moq-dev/moq#8857](https://github.com/moq-dev/moq/pull/8857) removed the earlier
pair: a min-delay decoder that showed stripes and drove the clock, and a
max-delay decoder a sweep behind whose clean pictures replaced them once it
caught up. The one-decoder path itself left before the cut with video
`warmup` (2026-11-16, see the [epic](/quest/m4/intra-refresh/README.md)) and
returns with [catalog warmup](/quest/m4/intra-refresh/catalog-warmup.md).
That pair decoded twice for the whole session, since the max-delay
lane never stops in steady state, to improve only the moments after a join or
skip. Its Rust and JS selection policies also differed (Rust held a sticky
source, JS picked per picture).

Measure, on a real intra-refresh import with the
[H.264 import](/quest/m4/intra-refresh/h264-import.md):

- How long a skip freezes with one decoder, against how long stripes show and
  how often the picture would switch sources with two.
- The steady-state CPU, power, and decoder memory a second decoder costs, on
  desktop and mobile hardware decoders.
- Whether Firefox and Safari open a codec on a non-IDR H.264 recovery point
  sent as `key`, with and without the zero-count recovery point
  `insertRecoveryPoint` inserts for Chromium. Playwright's Firefox build
  reports H.264 unsupported and its WebKit build would not launch on the Linux
  host, so check both with a real browser. Chromium refuses a non-IRAP H.265
  frame sent as `key`, so an HEVC refresh rendition fails through the
  decoder's `error` signal in either design.

A second decoder that only runs for one sweep after each join or skip, rather
than for the whole session, is the variant to measure first.

## Hosts

- linux - Chromium and Firefox
- macos - Safari and VideoToolbox

## Required

- [Catalog warmup](/quest/m4/intra-refresh/catalog-warmup.md) - the one-decoder players this measures against

## Related

- [H.264 import](/quest/m4/intra-refresh/h264-import.md) - publishes the real `warmup` for an end-to-end x264 check
- [Consumer warmup](/quest/m4/intra-refresh/consumer-warmup.md) - the transcoder decodes history so it never encodes stripes
