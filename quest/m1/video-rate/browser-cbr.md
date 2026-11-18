# [S] Keep a constant-bitrate rendition alive at a zero grant

## Goal

A `"constant"` rendition in `js/publish` (a browser and codec without an
honored per-frame quantizer, such as Firefox or Chrome's software H.264)
survives a zero bandwidth grant. The allocator gives one once higher priorities
take the estimate, and today `configure()` rejects bitrate 0 and fails the
rendition. Found in review of
[moq-dev/moq#9026](https://github.com/moq-dev/moq/pull/9026); the bug predates
it.

Non-goal: holding `"constant"` under the cap. It doesn't in practice (Chrome's
OpenH264 matches `"variable"`, libvpx and libaom overshoot at low grants, and
VideoToolbox drops frames), and the browser fallback doesn't need an accurate
hard cap.

## Plan

Decided with the maintainer on 2026-11-17
([comment](https://github.com/moq-dev/moq/pull/9026#issuecomment-6966230347)):

- ✅ Drop the hard-cap goal for the `"constant"` fallback (rejected: ranking
  codecs with an honored quantizer first, which would pick software VP9 over
  H.264 at about 1.5 to 2.2x the encode CPU; an outer feedback loop around
  `"constant"`, which reconfigures the encoder as output overshoots).

At a zero grant, hold every frame back without reconfiguring, counting them in
`skipped` as quantizer mode does, and configure again once the grant rises.
Cover it in `encoder.test.ts` with a grant that drops to 0 and recovers.
