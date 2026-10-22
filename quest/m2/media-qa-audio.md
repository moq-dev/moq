# [L] Media QA covers audio the way it covers video

## Goal

`just test media` grades audio changes as thoroughly as video changes. Video
has a codec change (AVC to VP8), a resize, and a late join; audio has only an
Opus tone and A/V sync, published from Chromium to Chromium. After this
quest, AAC playback, audio codec, sample-rate, and channel-count changes, and
an audio rendition switch are each graded for tone and A/V sync. Mute and
volume are checked on the output samples, and a native publisher's A/V sync
is graded.

## Plan

Decided in planning (2026-10-20):

- A native publisher's sync is graded through the Rust CLI: ffmpeg's filters
  draw the media QA frame-counter pattern and generate the stepped tone, and
  `moq` publishes them through moq-mux. The browser grades pitch and sync,
  which covers native importer timestamps. The FFI publishers' tone is
  graded for pitch and continuity by the interop matrix
  (`test/interop/tone.py`), not here.
- Browser AAC publish runs end to end for the first time in the AAC case.
- Watch mute and volume are verified on the output (muted is silence, volume
  scales the level). The media QA probe taps `audio.out.root`, upstream of the
  emitter's private volume `GainNode` (`js/watch/src/audio/emitter.ts`), so
  these cases need an observation point after that gain, where samples reach
  the speakers.
- `js/watch/src/audio/render-worklet.ts` gets a unit test; today only the
  browser lanes reach it.

Recommendations:

- Mirror the video cases' structure (`test/interop/clients/js/src/fixture.ts`)
  rather than adding a parallel harness.
- The ffmpeg pattern must match `src/pattern.ts` exactly; give it a negative
  control (an `-itsoffset` audio offset fails the sync check), like the
  existing silent-audio and audio-offset faults.
- A codec or format change mints a new rendition by the identity contract;
  check that the player follows it without a gap rather than asserting a
  particular switch mechanism.

Public API: none. Wire: none.

## Related

- [A/V clock](/quest/m1/av-clock.md) - changes what drives sync; these cases should pass before and after it
- [Audio outlives video](/quest/m2/audio-bufferbloat-av.md) - the same pattern publisher under bufferbloat
