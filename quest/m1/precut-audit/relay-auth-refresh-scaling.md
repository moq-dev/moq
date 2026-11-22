# [S] Reject sub-second auth revalidate cadences and fix per-connection O(K²) refresh rebuilds

## Goal

`moq-auth` rejects sub-second `revalidate` intervals so tokens never serialize
`"revalidate": 0` or bypass sweep cadence limits, and `moq-relay`'s
`Refresh::ended` avoids rebuilding every per-connection credential watch and
timer on every loop iteration when a single connection holds many credentials.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ place the high-severity auth cadence bug and
`moq-relay`'s `Refresh::ended` rebuild fix in `m1` (rejected: `m0` / `m3` /
`m4`).

1. **Reject `revalidate` below the minimum representable cadence
   (`rs/moq-auth/src/serve.rs:678-697`, `grant.rs:149-151`, `wire.rs:226-233`).**
   - `wire.rs` rounds `revalidate` to whole seconds when serializing, so a
     duration `< 500ms` (or `0`) serializes as `"revalidate": 0` and can bypass
     `Sessions::sweep` limits.
   - Reject `revalidate < 1s` (check wire rounding and existing `moq-auth`
     tests to confirm exact lower bound, `< 1s` vs `< 500ms`, ensuring no
     accepted value can ever serialize to `0`).
2. **Eliminate per-connection O(K²) `FuturesUnordered` rebuilds in
   `rs/moq-relay/src/auth/refresh.rs:368-396`.**
   - Each connection owns its own `Refresh` (`rs/moq-relay/src/auth.rs:410`),
     whose `self.credentials` map holds the `K` active credentials attached to
     that connection (not all sessions on the relay). On every loop iteration
     (`Event::Request`, `Verified`, `Settled`, `Expired`, or `Changed`),
     `Refresh::ended` drops and rebuilds both `changes` and `withdrawals`
     `FuturesUnordered` across all `K` credentials (re-registering each
     credential's `sleep_until` timer and close watch), making a `K`-token
     churn round on one connection O(K²).
   - Measure the credential-heavy axis (`tokens` sweep) using the existing
     `relay_auth_refresh` benchmark in `rs/moq-relay/benches/auth.rs:119-142`
     (which already sweeps `sessions` and `tokens` separately) before choosing
     the data structure; then replace the per-iteration rebuild with persistent
     per-credential watches and a single earliest-deadline timer (or the
     simplest O(log K) structure the profile justifies).

Public API: none (stricter validation of invalid zero/sub-second cadence;
internal relay refresh loop). Wire: none. Add unit tests for sub-second
`revalidate` rejection in `rs/moq-auth` and record before/after
`relay_auth_refresh` measurements across the `tokens` sweep in the PR.
