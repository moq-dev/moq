# [M] Open-GOP recovery drops orphaned leading pictures after each delivery break

## Goal

A viewer joining or resuming an open-GOP broadcast after a gap never hands the decoder a leading picture whose references it
does not have. Continuous groups keep every frame. The rule is the same for H.264
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
  it as `"key"`): for the first group after a gap item, skip delta frames
  stamped before that group's keyframe. `next()` yields `{ kind: "gap" }`
  before the first frame of a subscribe and after a latency skip, eviction, or
  hole; a declared end is not a gap, since a fresh encoder has no earlier GOP
  to reference. A latency skip does not flush the decoder, and a viewer that
  skipped into a later GOP lacks its references just like a cold join.
  Every group without a gap before it is passed through untouched.
- The same rule in the Rust decode path (`moq-video` decode consumers), so
  native playback and the transcoder tune in the same way, keyed on the
  container read's `Read::Gap` item.
- The same rule in `moq export ts`. Decided (2026-10-01): the fixed-delay
  export still sends a join's orphaned leading pictures, so 3 of 500
  frames on the open-GOP fixture decode after they present
  (`dts-before-pts`). Trim them at tune-in from the same signal, on the
  first group after each gap, and make
  `just test ts --open-gop` pass under
  `--strict`. Decided in the 2026-10-05 audit: the grader changes with it.
  `test/ts/open-gop.py`'s decode-order check (a contiguous run of source
  access units) and leading-pictures check, and the `test/ts/README.md` text
  that says the round-trip must hand every leading picture on, both expect
  every picture today; rewrite them to expect orphaned leading pictures
  trimmed after each delivery break and every continuous group's kept.
  The 2026-10-11 audit applies the same recovery rule to TS as to the
  playback paths: the previous first-group-only wording missed later skips.
  Rejected: dropping
  this bullet and keeping the grader.
- Tests: a synthetic group with a keyframe followed by two earlier-stamped
  deltas is trimmed on the first group and kept on the second; and a viewer
  that plays continuously, then latency-skips into a later open GOP, has that
  group's leading pictures trimmed too, so an implementation that only trims
  the initial group fails. Both cases in both languages.

Decided 2026-10-12: both container reads yield the gap item, so nothing
here waits on another quest.

## Related

- [Consumer warmup](/quest/m2/intra-refresh/consumer-warmup.md) - the `recovery_frame_cnt > 0` case this rule does not cover
- [Audio warmup](/quest/m1/audio-warmup.md) - keys its Opus pre-roll trim on the same signal
