# [M] The native audio lane runs the same profiles as the browser

## Goal

The same jitter profiles, the same budget file, and the same metric schema run
against `moq play` on a dummy audio device, so a divergence between the native
and browser playout paths shows up as a failing row rather than as a bug report
from whichever platform a user happened to be on.

## Plan

- Drive the shipped binary, not a purpose-built harness: `moq play --no-video`
  against a local relay behind the same seeded shaper. Headless playback has
  landed: `--no-video` opens no window or GPU surface, and `just test
  native-play` already hears it through a private PulseAudio null sink via the
  production ALSA/cpal backend in nightly (`test/native-play/`). Reuse that
  device setup rather than a fake clock: a real device callback is part of what
  is measured. Measure that null sink's timing noise before writing native
  budgets; it is the CI floor, not a physical device's.
- Collect the metrics where they happen. The native driver exposes only device
  failures and sink writes today, not rendered quanta, short quanta, stalls, or
  per-stage timings, so sample them at the device callback and decoder
  boundaries. Recommended shape: a bounded observation handle that a reader
  drains into the browser lane's `Summary` (`clients/js/src/schema.ts`), with
  overflow invalidating the run and no blocking or user callback on the audio
  thread. Settle the handle's API and the CLI JSON flag name when implementing.
- `moq play` emits that `Summary` as JSON for the analyzer. Keep it useful
  outside the test: a user debugging their own latency wants the same dump.
- Reuse the browser lane's analyzer and budget file (`test/audio-quality/`),
  with the JSON field names and meanings matching its schema exactly. Native
  rows get their own budget values in the same file, keyed the same way, since
  the device floor differs; they do not get their own schema. Never fill an
  unmeasured field with a passing zero.
- Add the lane to the nightly matrix beside the browser one.

Nothing here is a second estimator, and this lane does not compare the two
runtimes' target series: the [Audio jitter
target](/quest/m1/audio-jitter-target/README.md) line replays its recorded
trace through the native decode path and asserts the browser's target series
(2026-10-06 audit). This lane relies on that result; a disagreement there is
a finding against that line and never a reason to widen a budget. It grades
only end-to-end budgets, with the backend-dependent stages isolated: this
lane deliberately accepts real device callback noise, so a difference in
totals alone proves nothing about the estimator.

Standalone in m2 rather than a child of the m0 audio quality harness line
(decided in the 2026-09-28 quest audit): nothing in m0 waits on it. The native jitter target
it grades is done on the jitter target line.

## Related

- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - its native trace replay is the estimator comparison this lane relies on
