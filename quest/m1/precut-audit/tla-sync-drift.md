# [S] Sync TLA+ verification models and skill commit pointers with main

## Goal

Every `verify-*` skill points to a valid commit on `main`, `Handover.tla`
models the dialed-link drain changes from `#9158`/`#9213`, and
`SubscribeAccounting.tla` is re-verified against `#9280`/`#9307`/`#9308` with
TLC passing.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ fix TLA+ sync/model drift in `m1` ahead of the cut
(rejected: deferring to `m4`).

1. **Fix dangling pre-squash sync commits in verify skills:**
   - `.agents/skills/verify-reconnect/SKILL.md` (`4a6f522483`),
     `.agents/skills/verify-subscribe/SKILL.md` (`c145046a74`), and
     `.agents/skills/verify-credit/SKILL.md` (`92fe03e755`) record commit SHAs
     that do not exist on `main` (pre-squash SHAs), breaking `git diff
     <sync>..HEAD` drift checks.
   - Resolve each to the real merged commit on `main` after verifying the
     corresponding model against HEAD.
2. **Update `rs/moq-net/verify/reconnect/Handover.tla`:**
   - Audit the changes landed in `#9158` and `#9213` (dialed-link drain /
     handover behavior) against `Handover.tla`, mirror or explicitly document
     any abstraction delta, and run TLC via the `verify-reconnect` skill.
3. **Re-verify `rs/moq-net/verify/subscribe/SubscribeAccounting.tla`:**
   - Audit `#9280`, `#9307`, and `#9308` against `SubscribeAccounting.tla`,
     update the spec if any transition or invariant changed, and run TLC via
     the `verify-subscribe` skill.
4. **Re-check `verify-credit`:**
   - Confirm `verify-credit`'s model matches `HEAD` and record the valid
     post-squash `main` commit SHA.

Public API: none. Wire: none. Include TLC pass output (states explored, 0
violations) and updated sync SHAs in the PR.
