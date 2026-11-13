# [M] Audio recovers from a congestion stall quickly

## Goal

After a congestion stall, audio is back on the shared clock within about 2 s
of arrivals resuming, in the browser and natively, instead of trailing by up
to 1.4 s for 17 to 28 s. Audio that trails the clock by more than the
`stretch` threshold, now 200 ms by default, skips onto the clock, and the
`stretch` setting sizes nothing but that threshold.

## Plan

Found by the triage of Nightly run
[43447849289](https://github.com/moq-dev/moq/actions/runs/43447849289)
(2026-11-13). Caused by [moq-dev/moq#8507](https://github.com/moq-dev/moq/pull/8507),
which stopped trimming audio that trails the clock, and
[moq-dev/moq#8532](https://github.com/moq-dev/moq/pull/8532), whose `stretch`
bound defaults to 2 s. Chromium `opus-48000-congestion` `av_lag_ms` p95 is
1434 ms against a 32 ms ceiling; trims fell from about 1000 ms/min to 0. Both
players catch up at about 82 ms of lag a second, and each later fall of the
target adds lag back.

Decided 2026-11-13 (maintainer, /quest-plan): ✅ a regression to fix
(rejected: accept it and let calibrate budget the congestion rows). Then the
same day, in the [audio playout](/quest/m2/audio-playout/README.md) plan:
✅ a 200 ms default threshold past which audio skips onto the clock
(rejected: the implementer measuring a lower bound against NetEq's fast
accelerate). This is the first step of that rule, on today's splices; the
[lag rule](/quest/m2/audio-playout/lag-rule.md) replaces the controller
under it.

Scope:

- `stretch` defaults to 200 ms (`js/watch/src/sync.ts` `STRETCH`, `moq play
  --stretch`); zero still turns the stretch off.
- A lag past the threshold skips onto the clock, not back to the bound, in
  `js/watch/src/audio/ring-buffer.ts` and `rs/moq-audio/src/ring.rs`. A
  buffered player skips only past the larger of its lookahead and the
  threshold, as today.
- The setting sizes nothing else. The audio subscription keeps the delay
  plus the larger of the lookahead and the estimator's 2 s ceiling, so late
  frames are still observed and a buffered player keeps its lookahead.
  Since [moq-dev/moq#8611](https://github.com/moq-dev/moq/pull/8611) that max
  delay is also where buffered media past a missing group skips it, so the
  threshold must not shrink it. Natively, the decode cap, the hole policy (`fill_max`), and video's wait on
  a missing group (`args.rs` `video_max_delay`) stop following it, so video
  waits its own delay. That also fixes `--delay auto --stretch 0`, where any
  hole over 50 ms drops the sink and its queued audio (`play/media.rs`
  `cap`).
- The audio-quality harness reports, per row, how long after a disturbance
  audio is back within a frame of where it rested before it. Today's
  `recover_ms` measures against the window's p50 and is not tied to the
  disturbance.
- `doc/concept/audio-jitter.md`'s observation point and skip rules say the
  same.

Put the before and after rows of `just test audio-quality` for chromium and
native in the PR.

## Related

- [Resume sync](/quest/m0/interop-green/resume-sync.md) - snaps resumed audio to the clock, the same rule for a return from absence
