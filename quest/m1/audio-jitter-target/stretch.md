# [XL] Audio slows down and speeds up around its target in every player

## Goal

`moq play`, a live `<moq-watch>`, and a buffered one (`lookahead` above zero)
follow one decision rule, written once in `doc/concept/audio-jitter.md` and
held by one set of shared vectors:

- In steady state audio plays on the shared clock at the playout delay.
- When the audio received ahead of the playhead runs low, playback slows down
  and audio falls behind the clock.
- Once that buffer is back above the target, playback speeds up until audio is
  on the clock again.
- After an underrun, audio resumes on a small cushion and slows down until
  the buffer is back at the target.
- When the target moves, audio follows by stretching. It neither parks in
  silence on a rise nor skips buffered audio on a fall.
- Audio skips ahead only once it trails by more than the subscription's max
  delay still delivers.

So a congestion stall costs a gap and a catch-up, not the audio that arrived
late. A buffered player follows the same rule and never skips inside its
lookahead, which fixes the permanent lag and dropped audio groups of
[moq-dev/moq#8031](https://github.com/moq-dev/moq/issues/8031).

Boundaries: the estimator, the target formula, and video are unchanged. No
packet loss concealment. Zero underruns is not a goal. No new public setting:
the bound stays the max delay each player subscribes with today, until
[the `stretch` setting](/quest/m1/audio-jitter-target/stretch-setting.md).

## Plan

Planned 2026-11-09 (maintainer, /quest-plan). The
[epic](/quest/m1/audio-jitter-target/README.md) records the model and why.
Decisions for this quest:

- Refill after an underrun: ✅ resume on a small cushion, about one stretch
  window plus the render slack, then slow down to the target (rejected:
  refill to the full target, as the browser does today; measure both and pick
  in the PR). Refilling adds up to one target of silence and leaves audio
  that much further behind.
- Speed: ✅ NetEq's thresholds and rates, untuned, in both directions, with
  fast accelerate at four times the high limit (rejected: a few percent both
  ways; a capped slow-down with NetEq's catch-up). A few percent recovers a
  2 s lag in over a minute and saves about 1 ms near empty. The browser's
  fast accelerate at 20 ms of excess goes.
- Buffered mode: ✅ folded in here, and
  [moq-dev/moq#8360](https://github.com/moq-dev/moq/pull/8360) is closed
  (rejected: merge it as is and unify after; fix its four audit items first).
  Its branch `quest/m1/audio-jitter-target/buffered-converge` at `7683e4e076`
  anchors buffered audio to the shared clock and adds buffered replay rows;
  both are worth taking.
- Order: ✅ after native backpressure (rejected: this first; one quest for
  both). The level this rule reads is everything received ahead of the
  playhead, decoded or still encoded. The browser keeps the part of the
  target above its ring's floor encoded, and a buffered player its whole
  lookahead, while native decodes everything at once. One mirrored rule needs
  both players to split the buffer the same way.
- Clock step: ✅ deferred (rejected: plan it now; drop it). The shared
  clock's reference only moves earlier, so after a lasting path-delay step
  audio would settle behind a stale clock. Report what the `step` rows' lag
  does in the PR body so the follow-up is decided with numbers.

What the audit of `main` found (2026-11-09, `10772522a5`):

- Native (`rs/moq-audio/src/stretch.rs`, `decide`) expands whenever the raw
  depth is below the target, which is true between every arrival, so audio
  settled 60 to 88 ms behind. NetEq's `DecisionLogic` compares a filtered
  level against `low = max(3/4 target, target - 85 ms)` and
  `high = max(target, low + 20 ms)`; the port kept neither the filter nor
  the low limit.
- The browser (`js/watch/src/audio/ring-buffer.ts`, `stretch.ts`) never
  expands, fast-accelerates at 20 ms of excess, stays silent after an
  underrun until the whole target refills, parks on a target rise, and turns
  the stretch off in buffered mode.
- [moq-dev/moq#8361](https://github.com/moq-dev/moq/pull/8361) held native
  audio on the clock and took AAC `step` from 0 to 614 underruns a minute.
  Its own analysis: the clock's reference never moves later, so after the 5
  to 60 ms step the clock-aligned depth sits about 55 ms short of the
  estimator's target, less than AAC's 160 ms PES spacing, and `main` had
  hidden that by settling a burst behind. It already admitted a speed-up only
  inside the excess, so that rule alone prevents nothing here. Under this
  model the slow-down lets audio settle behind the stale clock rather than
  underrun, which is the lag the deferred clock decision is about.

Recommendations:

- NetEq assumes evenly paced packets. A batched publisher such as AAC in TS
  delivers a whole PES at once, so the depth drains by a burst between
  arrivals and its mean sits well under the target on a perfect path.
  NetEq's limits on that mean would settle audio behind, as native does
  today. Keep NetEq's limits, filter, and rates, and choose a level with the
  publisher's cadence removed: the depth right after each arrival less what
  the stretch changed since (the browser's `peak`), or the playhead's lag
  behind the shared clock. Keep a check on the raw depth too, since a stall
  delivers no arrival to decide on. The `bursty-aac` replay row holds this:
  its steady-state lag should match the Opus rows'.
- A target rise leaves audio ahead of the clock and a fall leaves it behind,
  so both become ordinary slow-down and speed-up. `AudioRingBuffer.park` and
  the skip on a lowered target have no caller left.
- Rewrite the unit tests that pin the old rule rather than work around them:
  `stretch.test.ts` ("never to slow down"), `ring-buffer.test.ts` ("re-stalls
  and refills to the target", "never expands", "underruns every burst ...
  rather than settling later", the 20 ms fast case), `replay.test.ts` ("never
  behind it", "rather than settling behind the clock"), and
  `stretch_tracks_both_clock_directions_and_target_changes` in
  `rs/moq-audio/src/ring.rs`. The shared corpus
  (`doc/concept/audio-jitter/stretch/corpus.json`) gains decision vectors for
  the new inputs.
- Re-measure every replay row in this PR; the rule changes all of them.
- Docs in the same PR: one decision section in `doc/concept/audio-jitter.md`
  replaces the native order and "Browser decision", its "Across renditions"
  sentence on parking and skipping goes, and the `lookahead` docs in
  `js/watch/src/element.ts`, `js/watch/src/sync.ts`, and
  `doc/lib/js/watch.md` say a buffered player catches up by playing faster.

Carried from the native port
([moq-dev/moq#7856](https://github.com/moq-dev/moq/pull/7856)): the
maintainer listened on 2026-11-05 and found the stretch inaudible on speech,
and it costs 16 to 43% of a callback, 1.02 ms of a 42.7 ms period at worst.
Rerun the playback benchmarks (`just rs bench-tests -p moq-audio --features playback
--run-ignored only -E 'test(benchmark_mixer_callbacks)'`) and `bun watch/bench/ring.ts`, since the
decision now runs a filter.

Public API: none expected. Wire: none.

## Closes

- [#8031](https://github.com/moq-dev/moq/issues/8031) - buffered playback loses lip sync and cuts audio out on a live stream

## Related

- [The `stretch` setting](/quest/m1/audio-jitter-target/stretch-setting.md) - makes this quest's skip bound configurable, and zero turns the stretch off
- [Audio outlives video under bufferbloat](/quest/m4/audio-bufferbloat-av.md) - the audio and video congestion rows this behavior is asserted on
