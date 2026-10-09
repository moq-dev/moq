# [M] Request churn costs the same with many held subscriptions

## Goal

Opening and closing a request on a session costs the same whether the
session holds 1 or 1,024 subscriptions. The `session_churn_held` benchmark
added by request caps (#4820) is flat in held subscriptions.

## Plan

Measured on request caps (#4820): about 70 µs per churn at held=1 and 10 to
20 ms at held=1024 across 4 sessions, equally on lite-06 and draft-22. Neither
has a request window, so the slope predates request caps. Profile first, find what
scans the held subscriptions on each churn, and make that path proportional
to what it touches. Keep the bench swept over both sessions and held
subscriptions.

Public API: none. Wire: none.

## Related

- [Relay profiling](/quest/m1/perf/lock-profile.md) - the `just` profiling recipe this can use
