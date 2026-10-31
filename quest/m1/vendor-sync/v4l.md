# [XS] Finish the moq-v4l vendor-sync review

## Goal

moq-v4l carries every relevant libv4l-rs fix since its vendored baseline,
records the newest upstream commit checked, and passes local checks and the
required other-vendor review before its PR becomes ready.

## Plan

Recurring child of [vendor sync](/quest/m1/vendor-sync/README.md).
Implementation is checkpointed in
[moq-dev/moq#7459](https://github.com/moq-dev/moq/pull/7459): all 19 upstream
commits after `v4l-0.14.0` were audited, unused control wrappers were removed,
all six remaining flag conversions retain unknown bits, and `VbiCapture` has
the upstream spelling. `rs/moq-v4l/README.md` records every disposition.

Last checked: upstream `master` at
[`30f6dbae`](https://github.com/raymanfx/libv4l-rs/commit/30f6dbaeed5cdb1b33760fc5b35c1b545cfe0e46)
(2024-10-05), checked 2026-10-31 along with published security advisories
(none). No upstream feature follow-ups were identified.

Remaining: complete local other-vendor review and address its findings, then
delete this quest and its link when the PR is ready. The review attempt on
2026-10-31 failed before producing findings because Claude's weekly quota
resets November 2 at 02:00 America/Los_Angeles. Do not treat that failed
attempt as a clean review.

Validation: the new flag test reproduced truncation before the fix; both
unit tests, all 17 doctests, and the full local `just ci` passed. The default
shell startup hook's mbx launcher failed with `Argument list too long`;
the same pinned Nix toolchain passed with that hook disabled for the command.
