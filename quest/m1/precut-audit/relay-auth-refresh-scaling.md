# [S] Reject sub-second auth revalidate cadences and eliminate O(N²) refresh rebuilds

## Goal

`moq-auth` rejects sub-second `revalidate` intervals so tokens never serialize
`"revalidate": 0` or bypass sweep cadence limits, and `moq-relay`'s
`Refresh::ended` handles session refresh events in O(log N) instead of
rebuilding all N futures and sleep timers per event.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ place the high-severity auth cadence bug and
`moq-relay`'s O(N²) `Refresh::ended` rebuild in `m1` (rejected: `m0` / `m3` /
`m4`).

1. **Reject `revalidate` below the minimum representable cadence
   (`rs/moq-auth/src/serve.rs:678-697`, `grant.rs:149-151`, `wire.rs:226-233`).**
   - `wire.rs` rounds `revalidate` to whole seconds when serializing, so a
     duration `< 500ms` (or `0`) serializes as `"revalidate": 0` and can bypass
     `Sessions::sweep` limits.
   - Reject `revalidate < 1s` (check wire rounding and existing `moq-auth`
     tests to confirm exact lower bound, `< 1s` vs `< 500ms`, ensuring no
     accepted value can ever serialize to `0`).
2. **Replace O(N²) `FuturesUnordered` rebuild in `rs/moq-relay/src/auth/refresh.rs:368-396`.**
   - On every refresh or session event, `Refresh::ended` currently drops and
     rebuilds a `FuturesUnordered` containing every active session's watch
     future plus a sleep timer, causing O(N²) allocations and timer-wheel churn
     under high connection counts.
   - Maintain persistent session watches and a single earliest-deadline timer
     (backed by a min-heap / `BTreeSet` keyed by `(deadline, id)`), updating
     only the affected session in O(log N).

Public API: none (stricter validation of invalid zero/sub-second cadence;
internal relay refresh loop). Wire: none. Add unit tests for sub-second
`revalidate` rejection in `rs/moq-auth` and a high-session-count regression
test / micro-benchmark check for `rs/moq-relay` auth refresh.
