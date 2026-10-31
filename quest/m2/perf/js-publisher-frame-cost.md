# [M] The JS publisher's per-frame cost stays flat with stream count

## Goal

The js/net publisher spends less time per frame on promise races, and its
per-frame cost at 1024 concurrent streams is close to its cost at 128.

## Plan

Found 2026-10-28 by [moq-dev/moq#7133](https://github.com/moq-dev/moq/pull/7133),
which cut each object to one header write plus one payload write.

- About 19% of the remaining time goes to three promise races per frame on the
  IETF path. Merging two of them touches cancellation, so keep a cancelled
  stream's behaviour and test it.
- Per frame, the cost is about 3x higher at 1024 streams than at 128. Find the
  structure that grows with stream count.

Decided 2026-10-28 (maintainer): leave out the timer-gap effect #7133 noted.
Browser timers get a turn less often because each stream now runs longer
between writes, and this bench's always-writable fake transport mostly causes
it.

Measure with `just js serve-bench` in the browser; Bun or Node timing is not
acceptance evidence.

Public API: none. Wire: none.
