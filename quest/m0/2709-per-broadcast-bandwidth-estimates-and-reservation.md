# [S] per-broadcast bandwidth estimates and reservation

## Goal

Implement and verify the behavior tracked in [#2709](https://github.com/moq-dev/moq/issues/2709)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

The congestion controller's `estimatedSendRate` (and the PROBE receive estimate) are per-session. With #2705 sharing one session across every component on a page, that aggregate is the wrong number for any individual publisher: two publish components each cap their encoder at the full-session estimate and jointly overshoot, and a watcher's ABR reads a budget it shares with everything else on the connection.

This is not a regression in kind (separate sessions just let congestion control arbitrate blindly), but sharing makes it structural. What is missing is a mechanism to split the session estimate across broadcasts: per-broadcast accounting at minimum, and ideally a way to reserve or prioritize bitrate so a publisher's encoder cap and a watcher's rendition selection each work against their own share rather than the whole pipe.

Related: #2283 fed the session estimate into the encoder; catalog::Estimator (#2530) measures per-rendition receive rates. Neither divides the send budget.

## Closes

- [#2709](https://github.com/moq-dev/moq/issues/2709) - close this issue when the quest finishes
