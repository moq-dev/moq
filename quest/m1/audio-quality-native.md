# [M] The native audio lane runs the same profiles as the browser

## Goal

The same jitter profiles, the same budget file, and the same metric schema run
against `moq play` on a dummy audio device, so a divergence between the native
and browser playout paths shows up as a failing row rather than as a bug report
from whichever platform a user happened to be on.

## Plan

- Drive the shipped binary, not a purpose-built harness: `moq play --no-video`
  against a local relay behind the same seeded shaper, on a cpal null or dummy
  backend so CI needs no audio hardware. The draft supplies the headless path
  through the real speaker without constructing a window. A real device callback
  is part of what is measured, so keep the production backend rather than a fake
  clock. Measure that timing noise before writing native budgets.
- `moq play` needs to emit the run's counters and stage timings as JSON for the
  analyzer. Add that output, and keep it useful outside the test: a user
  debugging their own latency wants the same dump.
- Reuse the browser lane's analyzer and budget file (`test/audio-quality/`, the
  schema is `clients/js/src/schema.ts`), with the JSON field names
  and meanings matching its schema exactly. Native rows get their own budget
  values in the same file, keyed the same way, since the device floor differs;
  they do not get their own schema.
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

Standalone in m1 rather than a child of the m0 audio quality harness line
(decided in the 2026-09-28 quest audit): nothing in m0 waits on it. The native jitter target
it grades is done on the jitter target line.

### Draft progress

The draft routes `moq play --no-video` through an async event receiver instead
of constructing winit or a wgpu surface. It still opens the production
moq-audio Engine and uses the same Media tasks. Native playback tests cover
the actual dispatch and transport-error propagation, and the existing native
playback suite passes.

The metric lane remains unfinished. The native driver exposes device failures
and sink writes, not the browser schema's rendered quantum, short-quantum,
stall, and stage series. Those measurements must be collected at the device
callback and decoder boundaries before emitting compatible JSON. No fake zero
metrics, unmeasured budget values, or passing native nightly row are added.
Measure the real dummy-device floor, then complete JSON output, analyzer
integration, and nightly wiring here.

## Related

- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - its native trace replay is the estimator comparison this lane relies on
