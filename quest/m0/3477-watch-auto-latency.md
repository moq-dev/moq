# [M] js/watch: auto latency follows measured arrivals, and the audio ring holds slack and re-buffers

## Goal

The "Real-time" / auto preset plays clean audio on a LAN and against a public
relay: no underrun episodes and no skip-aheads in steady state, at the lowest
delay the measured arrivals allow. The target is sized from what actually
arrives, the way WebRTC's NetEq does, and the round-trip time drops out of
the formula. The audio ring tolerates one chunk of slack above the target and
re-buffers after an underrun instead of playing the next chunk on an empty
cushion. Audio underruns are visible in the stats panel.

Boundaries: convergence still uses skip-ahead and silence; time-stretching is
[Time stretch](/quest/m2/watch-audio-time-stretch.md). The TS importer's PES
bursts are advertised honestly by the jitter estimator quest, not re-paced.
The publisher-side encoder lag observed in the same report is its own quest.

## Plan

Branch from main. The ring files are byte-identical on both branches; only
`sync.ts` differs (dev's `delay` + `buffer` split from #3396), so the target
estimator lands in whichever shape the branch has and merges forward.

Measured in #3477 (traces and a replay harness on the reporter's fork): with
a 46 ms auto target and two-chunk pacing the shipped ring produces hundreds
of underruns and skips per minute; holding one chunk of slack and re-buffering
removes every underrun at the same target; only a target that follows the
arrival spread (270 ms on the public bursty stream) removes the skips. A
fixed 250 ms is clean everywhere, which is why 500 ms and 1 s always sounded
fine.

### Target

- Measure arrival jitter per track at container frame arrival in `js/hang`,
  before decode: arrival wall time minus media timestamp, relative to its
  running minimum. Keep a decaying histogram and read a high percentile
  (p95 to start, NetEq's forgetting-factor shape), floored at one frame plus
  the catalog `jitter`. `Sync` takes the maximum across audio and video and
  adds nothing RTT-derived; `MIN_JITTER` and the `1.25 x minRtt` term go.
- The target starts from the catalog floor and converges as samples arrive;
  it only lowers gradually, so a viewer's buffer shrinks in steps of at most
  one chunk per interval and never underruns on a refinement.
- `maxAge` on the wire is `delay + buffer` as today; #3478 makes the
  fractional case safe on dev.

### Ring

- `shared-ring-buffer.ts` `read()`: skip only when buffered exceeds
  `latency + chunk`, landing at `latency`; raise `STALLED` when the ring runs
  dry on a non-empty timeline so `insert()` refills to the target before
  playback resumes. That is a new worklet-to-main-thread write of the flag;
  state the atomics discipline next to the existing epoch comments.
- `ring-buffer.ts` (the postMessage path every non-isolated page uses):
  decouple capacity from the target in `#capacityFor`, apply the same band
  in `write()` overflow, and set `#stalled` on underrun.
- `buffer.ts`: re-stalling flips `Backpressure.wait` in buffered mode, which
  starts flushing waiters mid-playback. Reconcile with
  `decoder.ts` `#runLatencyReanchor`, which already resets on a floor
  increase, so there is one re-buffer path.
- Render underrun shortfall with a short ramp instead of a hard step.

### Visibility

- `audio.out.stalled` and an underrun counter reach the stats panel and the
  buffering indicator, which today read video only; `demo/web` stops
  hardcoding `stalled=false` for the audio bar.

### Verification

- Unit: replay the recorded arrival traces (attach a trimmed copy under
  `js/watch`) through both rings and assert zero underruns at the converged
  target; synthetic Gaussian arrivals with two-chunk bursts converge within a
  bound.
- Browser: the `test/smoke` JS driver plays `bbb` from a local `moq import
  ts` with ffmpeg's default PES packing at auto and asserts underruns stay
  at zero after convergence, on the isolated and the postMessage path.
- Manually against the public relay on Chrome and Safari, the two rows the
  issue measured.

## Closes

- [#3477](https://github.com/moq-dev/moq/issues/3477) - close this issue when the quest finishes
- [#2812](https://github.com/moq-dev/moq/issues/2812) - the iOS stutter report this reproduces on desktop; close it with this

## Related

- [Time stretch](/quest/m2/watch-audio-time-stretch.md) - convergence without skips or silence, on top of this
- [Jitter estimator](/quest/m0/3479-mux-jitter-flush-span.md) - an honest catalog floor for bursty importers
- [Audio identity](/quest/m0/3479-watch-audio-identity.md) - stops catalog churn resetting the ring
- [Browser media QA](/quest/m0/browser-media-qa.md) - the harness that measures playback output
