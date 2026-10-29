# [XS] Remeasure the planned wire break

## Goal

`just test wire-compat` runs again: the lite-07-wip planned break in
`test/interop/compat/planned-breaks.json` is measured against the released
`@moq/net` 0.4.6 instead of 0.4.5.

## Plan

npm published `@moq/net` 0.4.6 on 2026-10-23, so `stale()` in
`test/interop/compat/resolve.ts` refuses every scheduled run since. Run
`just test wire-compat --remeasure`, confirm the lite-07-wip cells still
fail against the released builds and the stable lite-01 through lite-06
cells still pass, and update the entry's releases and reason with the date.

Public API: none. Wire: none.
