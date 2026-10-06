# [M] Request churn costs the same with many held subscriptions

## Goal

Opening and closing a request on a session costs the same whether the
session holds 1 or 1,024 subscriptions. The `session_churn_held` benchmark
proposed by request caps (#4820) is flat in held subscriptions.

## Plan

Blocked until request caps (#4820) lands with the benchmark. Measured on
that PR: about 70 µs per churn at held=1 and 10 to 20 ms at held=1024 across
4 sessions, equally on lite-06 and draft-22. Neither has a
request window, so the slope predates request caps. Profile first, find what
scans the held subscriptions on each churn, and make that path proportional
to what it touches. Keep the bench swept over both sessions and held
subscriptions.

Public API: none. Wire: none.

## Required

- [Request caps](/quest/m0/request-caps.md) - the held-subscription churn benchmark exists on main

## Related

- [Reproducible relay CPU and allocation profiles](/quest/m1/performance-profiles.md) - the profiling setup this can use
