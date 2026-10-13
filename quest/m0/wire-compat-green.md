# [S] Released wire compatibility runs again on main

## Goal

Interop's `Released wire compatibility` step (`just test wire-compat`) runs
its whole matrix against the newest releases on `main`'s nightly. Every cell
passes or is skipped by a planned break the maintainer re-accepted against
those exact releases.

## Plan

Two independent causes, both on the scheduled Interop run of 2026-10-12
(38284317921) and earlier:

- Stale planned breaks. A release went out with moq-cli 0.14.3, moq-relay
  0.17.3, hang 0.21.12, and @moq/net 0.4.3. Every entry in
  `test/interop/compat/planned-breaks.json` pins the previous releases, so
  `resolve.ts` aborts with `stale planned break: moq-lite-07-wip, moq-cli
  changed from 0.14.2 to 0.14.3` before any cell runs. That is the tripwire
  working as designed. Re-measure each entry against the new releases: delete
  an entry whose break is gone (the `catalog-archive` reshape and the released
  relay's FETCH hang may have shipped fixed), and refresh the pinned versions
  and reason of one that persists. Get the maintainer's acceptance in the PR,
  as the existing reasons record.
- The released JS lanes run this checkout's client. `compat.sh` copies
  `test/interop/clients/js-native/subscribe.ts` into the released `@moq/*`
  environment. #5099 (2026-10-10) changed that script to call
  `catalog.latest()`, which released `@moq/hang` 0.5.2 lacks. Every
  `publisher=released fetch` cell has failed with `catalog.latest is not a
  function` since the 2026-10-11 nightly. The released environment needs a
  client that compiles against the released API. Recommended: give the compat
  lanes their own observer in `test/interop/compat/client.ts`, which is
  already the client meant for both environments. Stop sharing the interop
  client, whose API follows the checkout. The alternative is to stage the
  client from the released package's git tag, but that ties the compat run
  to old harness flags.

Run `just test wire-compat` locally (it needs `gh` registry access) before
and after. Don't dispatch the nightly on a branch.

Public API: none. Wire: none.

## Related

- [#5099](https://github.com/moq-dev/moq/pull/5099) - the API change the shared client picked up
- [#4728](https://github.com/moq-dev/moq/pull/4728) - the released wire-compat harness and its planned-break tripwire
