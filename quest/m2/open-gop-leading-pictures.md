# [M] Open-GOP leading pictures are dropped at tune-in only

## Goal

A viewer joining an open-GOP broadcast at a recovery point never hands the
decoder a leading picture whose references it does not have, while a viewer
already playing keeps every frame. The rule is the same for H.264
recovery-point keyframes with `recovery_frame_cnt = 0` (the broadcast
contribution case, and what the reference capture carries) and H.265 CRA
pictures. Gradual recovery (`recovery_frame_cnt > 0`) is out of scope: the
splitter does not retain the count, and unsafe pictures there can sit at or
after the keyframe timestamp, so it has its own quest,
[gradual recovery](/quest/m2/open-gop-gradual-recovery.md).

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
  it as `"key"`): for the first group after any non-continuous transition,
  skip delta frames stamped before that group's keyframe. That covers a
  subscribe, a declared discontinuity, and a latency skip: `#checkLatency`
  records the skip through `#gap` and `next()` reports the next frame with
  `continuous: false` without touching the discontinuity counter, and a viewer
  that skipped into a later GOP lacks its references just like a cold join.
  Every continuous group is passed through untouched.
- The same rule in the Rust decode path (`moq-video` decode consumers), with
  an equivalent non-continuous signal from `container::Consumer`, so native
  playback and the transcoder tune in the same way.
- Tests: a synthetic group with a keyframe followed by two earlier-stamped
  deltas is trimmed on the first group and kept on the second; and a viewer
  that plays continuously, then latency-skips into a later open GOP, has that
  group's leading pictures trimmed too, so an implementation that only trims
  the initial group fails. Both cases in both languages.

## Required

- [#2067](/quest/m2/2067-test-open-gop-h-264-tune-in-end-to-end-leading-picture.md) - decides whether the glitch is dropped frames, corrupt frames, or a decoder error, which sets what this has to prove

## Related

- [Gradual recovery](/quest/m2/open-gop-gradual-recovery.md) - the `recovery_frame_cnt > 0` case this rule does not cover
