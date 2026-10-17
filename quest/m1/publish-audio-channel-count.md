# [M] Forcing a channel count on Audio.Capture does not cost audio

## Goal

`new Audio.Capture({ source, channelCount: 1 })` publishes audio a subscriber
plays as smoothly as the same capture without the override, so pinning a
publisher's channel count stops being a way to degrade it. The browser media
fixture can then pin mono again.

This is also the home for the media fixture's cold-start audio-tone flake
without the pin, which shows the same symptom. Fix it at its cause, not by
lowering the tone floor.

`channelCount` keeps its meaning, "force the captured channel count".

## Plan

What is known:

- The real-worklet matrix in
  `test/interop/clients/js/capture-channel-count.browser.ts` (in
  `just test harness`) finds nothing override-specific: no timestamp gaps,
  the requested format, and audible output in every channel, on Linux and
  macOS. It only asserts each channel is audible at least once. An uncommitted
  per-quantum trace (#5270) found one short silent run during startup in
  every case, the no-override baseline included, and none after. Under heavy
  load the baseline also showed a later run, so a strict per-quantum check
  flakes on the source. Tightening the matrix needs a baseline-relative
  measure.
- The player stalls. In CI (#5270), the pinned fixture failed the `just test media`
  audio-tone check in 2 of 3 runs, against ~5% of unpinned runs (5 of 108,
  all cold start). Failures look the same either way: player samples go
  `stalled` while received audio bytes keep growing, and the jitter delay jumps
  from ~77 ms to 500-700 ms.

The working hypothesis is that the explicit downmix (`channelCountMode:
"explicit"` in `js/publish/src/audio/capture.ts`) changes something the player
reacts to, such as frame cadence or burstiness, rather than dropping PCM.
Neither half is proven. Measure the
publisher's frame arrival timing pinned vs unpinned before picking a fix, and
check whether the player's re-stall behavior
([watch jitter target](/quest/m1/audio-jitter-target/watch.md)) is the
amplifier. A loaded local host fails the media fixture either way, so compare
on CI or a quiet machine.

Not covered: an applied `getUserMedia` channelCount constraint also takes the
explicit path, but a destination-node track refuses that constraint, so the
harness cannot exercise it.

## Related

- [Watch jitter target](/quest/m1/audio-jitter-target/watch.md) - a ring that runs dry re-stalls and refills to the target
- [Test flakes, round 2](/quest/m1/test-flakes-2/README.md) - load flakes fixed at their cause
