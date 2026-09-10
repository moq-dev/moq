# [M] The native audio lane runs the same profiles as the browser

## Goal

The same jitter profiles, the same budget file, and the same metric schema run
against `moq play` on a dummy audio device, so a divergence between the native
and browser playout paths shows up as a failing row rather than as a bug report
from whichever platform a user happened to be on.

## Plan

- Drive the shipped binary, not a purpose-built harness: `moq play` against a
  local relay behind the same seeded shaper, on a cpal null or dummy backend so
  CI needs no audio hardware. A real device callback is part of what is being
  measured, so keep the real backend rather than substituting a fake clock, and
  accept that the timing noise it adds sets the floor for the native budgets.
  That floor is worth measuring on its own before the budgets are written.
- `moq play` needs to emit the run's counters and stage timings as JSON for the
  analyzer. Add that output, and keep it useful outside the test: a user
  debugging their own latency wants the same dump.
- Reuse the browser lane's analyzer and budget file, with the JSON field names
  and meanings matching its schema exactly. Native rows get their own budget
  values in the same file, keyed the same way, since the device floor differs;
  they do not get their own schema.
- Add the lane to the nightly matrix beside the browser one.

Nothing here is a second estimator. Compare the two runtimes at the estimator
first: the target series each produces from the same arrival trace, which is
the conformance corpus's own comparison and carries no device timing in it. A
disagreement there is a finding against [Audio jitter
target](/quest/m0/audio-jitter-target/README.md) and never a reason to widen a
budget. Only then compare end-to-end totals, with the backend-dependent stages
isolated: this lane deliberately accepts real device callback noise, so a
difference in totals alone proves nothing about the estimator.

## Required

- [Browser](/quest/m2/audio-quality-harness/browser.md) - defines the metric schema, the budget file, and the extracted shaper
- [Native jitter target](/quest/m0/audio-jitter-target/native.md) - the estimator this lane grades and compares against the browser; without it there is no target series and the budgets would be set against a playout path that holds nothing
