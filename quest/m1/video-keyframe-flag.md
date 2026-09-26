# [S] Encoded video knows its keyframes

## Goal

`moq_video::encode::Encoded` says whether an access unit is a keyframe, so the
capture `Control::cut()` throttle counts every keyframe, including the
encoder's own GOP cadence. Today it sees only forced and opening keyframes, so
a cut requested just after a cadence keyframe still forces another one.

## Plan

- Every backend already knows whether it produced a keyframe; carry it on
  `Encoded`. Whether that is additive depends on whether `Encoded` is
  `#[non_exhaustive]`; if it is not, this goes to `dev`.
- The throttle in the capture driver treats any keyframe as satisfying a
  pending cut and restarting the spacing window, matching what JS already does.
- Test with a backend whose GOP produces a keyframe right before a requested
  cut, asserting no extra keyframe.

Public API: one field or accessor on `Encoded`. Wire: none.

## Related

- [#4184](https://github.com/moq-dev/moq/pull/4184) - the `Control::cut()` this completes
