# [M] Open-GOP leading pictures are dropped at tune-in only

## Goal

A viewer joining an open-GOP broadcast at a recovery point never hands the
decoder a leading picture whose references it does not have, while a viewer
already playing keeps every frame. The rule is the same for H.264
recovery-point keyframes and H.265 CRA pictures.

## Plan

Leading pictures are decoded after the keyframe and presented before it, so
they are exactly the frames of a group whose timestamp is below the group's
keyframe timestamp. That makes them identifiable from the container timestamps
alone, with no POC parsing and no change to the splitter, which is why the
drop belongs on the consumer: ingest cannot know whether a given viewer has the
previous GOP, and dropping at ingest would degrade continuous playback for
everyone.

- In the JS consumer (`js/hang/src/container/consumer.ts` forces the first
  sample of a group to `keyframe`, and `js/watch/src/video/decoder.ts` submits
  it as `"key"`): for the first group decoded after a subscribe or a
  discontinuity, skip delta frames stamped before that group's keyframe. Every
  later group is passed through untouched.
- The same rule in the Rust decode path (`moq-video` decode consumers), so
  native playback and the transcoder tune in the same way.
- Tests: a synthetic group with a keyframe followed by two earlier-stamped
  deltas is trimmed on the first group and kept on the second, in both
  languages.

## Required

- [#2067](/quest/m0/2067-test-open-gop-h-264-tune-in-end-to-end-leading-picture.md) - decides whether the glitch is dropped frames, corrupt frames, or a decoder error, which sets what this has to prove
