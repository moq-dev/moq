# [M] Open-GOP leading pictures are dropped at tune-in only

## Goal

A viewer joining an open-GOP broadcast at a recovery point never hands the
decoder a leading picture whose references it does not have, while a viewer
already playing keeps every frame. The rule is the same for H.264
recovery-point keyframes with `recovery_frame_cnt = 0` (the broadcast
contribution case, and what the reference capture carries) and H.265 CRA
pictures. Gradual recovery (`recovery_frame_cnt > 0`) is out of scope: unsafe pictures
there sit at or after the keyframe timestamp, and the catalog `warmup` rule in
[consumer warmup](/quest/m2/intra-refresh/consumer-warmup.md) covers them.

## Plan

Leading pictures are decoded after the keyframe and presented before it, so
they are exactly the frames of a group whose timestamp is below the group's
keyframe timestamp. That makes them identifiable from the container timestamps
alone, with no POC parsing and no change to the splitter, which is why the
drop belongs on the consumer: ingest cannot know whether a given viewer has the
previous GOP, and dropping at ingest would degrade continuous playback for
everyone.

Measured through `<moq-watch>` in Chromium 153 on macOS, on the clip
`just test ts --open-gop` generates (three leading pictures per recovery
point) stretched to 120 s. Continuous playback decoded every leading picture
on both decoder paths, with no errors, and the two paths' frames were
identical. At a cold join the paths differ. VideoToolbox (the default there)
outputs nothing for the orphaned leading pictures and raises no error, and
every frame it does output matches the continuous decode, so a viewer merely
starts at the keyframe. The software decoder (`prefer-software`, the path a
browser without hardware H.264 takes; Linux was not measured) raises
`EncodingError` in every join, and the watch then closes its decoder, so video
stops. The same joins with the leading pictures removed from the stream decode
cleanly in software, so the error is the leading pictures, not the non-IDR
keyframe. A latency skip ("skipping slow group") orphans the next group's
leading pictures in the same way. The trim therefore has to happen before
decode, and the JS test should drive the software decoder.

- In the JS consumer (`js/hang/src/container/consumer.ts` forces the first
  sample of a group to `keyframe`, and `js/watch/src/video/decoder.ts` submits
  it as `"key"`): for the first group after any non-continuous transition,
  skip delta frames stamped before that group's keyframe. That covers a
  subscribe, a declared discontinuity, and a latency skip: `#checkMaxAge`
  records the skip through `#gap` and `next()` reports the next frame with
  `continuous: false`. Latency skip also bumps playhead generation (startup
  delay) but does not flush the decoder. Leading pictures after that
  non-continuous transition are still skipped, as above; a viewer that skipped
  into a later GOP lacks its references just like a cold join.
  Every continuous group is passed through untouched.
- The same rule in the Rust decode path (`moq-video` decode consumers), so
  native playback and the transcoder tune in the same way.
- The same rule in `moq export ts`. Decided (2026-10-01): the fixed-delay
  export (#4645) still sends a join's orphaned leading pictures, so 3 of 500
  frames on the open-GOP fixture decode after they present
  (`dts-before-pts`). Trim them at tune-in from the same signal, and make
  `just test ts --open-gop` pass under `--strict`.
- This quest owns the Rust non-continuous signal, which audio warmup and
  consumer warmup reuse rather than each adding one. Today
  `moq_mux::container::Consumer::poll_read` returns a bare frame, and
  `discontinuity()` is a counter bumped on a declared marker group, an
  unproven delivered hole, or a latency skip, but not on the subscribe itself.
  Add the equivalent of JS `continuous`: false on the first frame after the
  subscribe and after every bump, true otherwise. It changes the moq-mux
  consumer API, so pick main or dev by whether the shape is additive.
- Tests: a synthetic group with a keyframe followed by two earlier-stamped
  deltas is trimmed on the first group and kept on the second; and a viewer
  that plays continuously, then latency-skips into a later open GOP, has that
  group's leading pictures trimmed too, so an implementation that only trims
  the initial group fails. Both cases in both languages.

## Related

- [Consumer warmup](/quest/m2/intra-refresh/consumer-warmup.md) - the `recovery_frame_cnt > 0` case this rule does not cover
- [Audio warmup](/quest/m1/audio-warmup.md) - keys its Opus pre-roll trim on the same signal
