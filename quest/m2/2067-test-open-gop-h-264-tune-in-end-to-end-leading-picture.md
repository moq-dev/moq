# [M] Open-GOP H.264: fixture and cold tune-in characterization

## Goal

An open-GOP H.264 broadcast (non-IDR recovery-point keyframes with leading
pictures) round-trips in the `test/ts` harness as a regression fixture, and
the cold tune-in behaviour in a WebCodecs viewer is measured and written down:
glitch duration, dropped versus corrupt frames, and whether any decoder errors.

## Plan

#2066 taught the H.264 splitter to treat a recovery-point SEI as a keyframe,
so groups start at the recovery point and the rendition resolves. The
reference capture from #2050 is the hard case: every recovery point is
followed in decode order by about seven pictures presented *before* it, which
reference the previous GOP. At a cold tune-in those leading pictures decode
against missing references, and nothing in the Rust splitter or the JS
consumer identifies them. Continuous playback across a recovery point has all
the references and should be clean.

- Fixture: `rs/moq-mux/src/container/ts/test_data/scte35/kyrion_dirtystart.ts`
  is already open GOP with B-frames; confirm it carries leading pictures
  (POC below the keyframe after a recovery point) and use it, else generate a
  clip with x264 `open-gop=1` and a recovery-point SEI. The generated clip in
  `test/ts/run.sh` is closed-GOP IDR today, so add the open-GOP source as a
  second round-trip rather than replacing it.
- Characterization: there is no browser playback harness (`test/wasm` is
  transport-only), so measure by hand through `<moq-watch>` in Chrome:
  continuous playback across recovery points, cold tune-in at one, and a
  hardware decoder if available. Record the numbers in the PR and in the
  leading-picture quest.
- The consumer-side fix is
  [Open-GOP leading pictures](/quest/m2/open-gop-leading-pictures.md), which
  waits on these numbers.

## Closes

- [#2067](https://github.com/moq-dev/moq/issues/2067) - close this issue when the quest finishes

## Related

- [Open-GOP leading pictures](/quest/m2/open-gop-leading-pictures.md) - the tune-in drop this characterizes
