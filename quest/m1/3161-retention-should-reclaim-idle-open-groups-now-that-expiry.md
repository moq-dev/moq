# [S] Retention should reclaim idle open groups now that expiry is timestamp-only

## Goal

Implement and verify the behavior tracked in [#3161](https://github.com/moq-dev/moq/issues/3161)
within the issue's stated scope and boundaries.

## Plan

Rescoped during the 2026-08 grooming: #3169 moved the wall-clock LRU into the
cache pool on dev, but reclamation fires only on the track's next write, the
latest group is protected, and the JS prune still retains open groups.
Remaining: a write-independent timer for idle open groups, plus JS coverage.

### Issue context

\#3099 split group lifetime into two rules: subscription expiry is timestamp-only (a group is stale once its reach falls a full max-age budget behind the newest frame of the latest group), and wall-clock reclamation of idle content belongs to the retention/cache layer.

The expiry half landed in #3099. This issue tracks the reclamation half, which currently has gaps at the model layer:

- The Rust `cache::Pool` ages by last access, but an in-process track without a pool has no wall-clock reclamation at all, and a group held open by a stalled publisher pins its buffer until something else closes it.
- The JS track retention prune deliberately retains open groups, so an abandoned open group survives `Publisher Max Age` indefinitely.

Concrete symptom (from Codex's adversarial review of #3099): a reader drains an open group whose successor starts less than the budget ahead in media time, production goes quiet, and the pending read stays parked forever. Timestamp expiry is correct to keep the group (nothing proves it is useless), so the bound has to come from retention: an idle open group past the retention window should be reclaimed (aborted), which surfaces to a parked reader as the gap it is.

Needs a decision on what "idle" means for an open group (no producer writes for the retention window is the obvious candidate), a timer/wakeup to arm it, and coverage in both Rust and JS. The draft's Expiration section already frames retention as the publisher's own policy, so this is implementation work, not a wire change.

## Closes

- [#3161](https://github.com/moq-dev/moq/issues/3161) - close this issue when the quest finishes
