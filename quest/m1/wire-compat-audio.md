# [M] Wire-compat media lanes check the audio tone

## Goal

`just test wire-compat`'s media lanes prove audio crosses releases the way
`just test interop` proves it within the checkout: the current and released
Rust CLIs publish the interop tone, and the released and current Rust and JS
readers decode it and pass `test/interop/tone.py`. A garbled decode, a missing
audio rendition, or a timestamp gap between versions fails the lane.

## Plan

Split from the interop audio work, which checks the tone in every in-tree cell
but skips audio when `interop.sh` runs released binaries or JS clients
(`AUDIO=0`): the lanes keep the video-only Annex-B publisher and readers.

Things to look out for:

- The Rust readers export fMP4. Decoding its audio with ffmpeg, as the lanes
  already decode video, still owes tone.py the frame timestamps it checks for
  gaps.
- `compat/client.ts` runs against both package sets, so its audio decode can
  only use API the released `@moq/hang` has.
- Released `@moq/hang`'s libav fallback garbles every channel past the first
  when it converts interleaved PCM to planar (fixed alongside the interop audio
  checks). Until a release carries that fix, a released JS reader fails a stereo
  tone: decide between a planned break and starting mono.

Public API: none. Wire: none.

## Related

- [Interop AAC](/quest/m1/interop-aac.md) - adds an AAC track to the in-tree matrix; these lanes would follow
