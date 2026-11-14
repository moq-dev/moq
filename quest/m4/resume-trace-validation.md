# [L] Resume sweeps are checked against the TLA+ model

## Goal

Every case of the resume sweep (`rs/moq-net/src/model/resume_model.rs`) is
also a behaviour of `ResumeReader.tla`: a change to how a reader resumes
across route switches that the model does not allow fails the sweep, without
anyone remembering to resync the model.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan on what could be proved about
moq-net): every TLA+ model gets trace validation, not route alone, so none
can keep passing on code that has moved on. Milestone m4, behind route's.

- Reuse the trace format and the TLC recipe
  [route trace validation](/quest/m3/route-trace-validation.md) builds. The
  sweep records each step as the model action it corresponds to (the
  `verify-resume` skill's Coverage table is the mapping) with the variables
  the model keeps, and TLC checks the trace is a behaviour of the spec.
- The sweep covers more than the model does (ranges, cache expiry, a second
  instance). Restrict the validated cases to what the model covers or grow
  the model, whichever costs less, and say which.
- Before starting, check [Resume as producer replacement](/quest/m4/resume-redesign.md):
  it deletes the per-reader path this model describes. If it has landed or is
  in flight, validate against the redesign's model instead, or delete this
  quest with the old one.
- Nightly on the sweep's fresh seed (`test resume-model`); the merge gate
  only if it stays fast on the fixed cases.

Public API: none. Wire: none.

## Required

- [Route sweeps are checked against the TLA+ model](/quest/m3/route-trace-validation.md) - the trace format and TLC recipe this reuses

## Related

- [Resume as producer replacement](/quest/m4/resume-redesign.md) - rewrites the code this validates
