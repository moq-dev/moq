# [S] Derived origins borrow

## Goal

Closing a `@moq/net` origin handle derived with `scope()` releases only that
handle; only the owner closes the origin.

## Plan

Decided 2026-10-22 (API audit). Verified: `origin.scope(...).close()` closes
the owner (`js/net/src/origin.ts`), undercutting the `Table` interface, which
exists so a borrower cannot close the origin. `withPreference` is removed by
[moq-net exports](/quest/m1/api-audit/net-exports.md).

- A derived handle's `close()` releases what that handle registered.
- moq.pro relies on the old behavior in `e2e/src/relay-stats.ts` and
  `dev/smoke.ts`, where the scoped handle is the only one kept; note the
  change in the Impact bullet so the upgrade keeps the owner.

Check Rust's derived handles for the same behavior and match.

Public API: `@moq/net` behavior change. Wire: none.
