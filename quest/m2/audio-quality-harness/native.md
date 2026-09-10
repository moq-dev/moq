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
- Reuse the browser lane's analyzer and budget file. Native rows get their own
  budgets in the same file, since the floor differs, but not their own schema.
- Add the lane to the nightly matrix beside the browser one.

Nothing here is a second estimator. If the native and browser numbers disagree
beyond the profiles' tolerance, that is a finding against
[Audio jitter target](/quest/m0/audio-jitter-target/README.md), not a reason to
widen a budget.

## Required

- [Browser](/quest/m2/audio-quality-harness/browser.md) - defines the metric schema, the budget file, and the extracted shaper
