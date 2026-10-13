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
  `catalog.latest()` on a `Json.Snapshot.Consumer`, a method released
  `@moq/json` 0.4.2 lacks (`js/json/src/snapshot/consumer.ts`). Every
  `publisher=released fetch` cell has failed with `catalog.latest is not a
  function` since the 2026-10-11 nightly. Decided by the maintainer
  2026-10-13 on #5388: the compat lanes run their own observer in
  `test/interop/compat/client.ts`, using only API the released packages
  have, and stop sharing the interop client, whose API follows the checkout.
  Staging the client from the released package's tag was rejected, since it
  ties the compat run to old harness flags.

Run `just test wire-compat` locally (it needs `gh` registry access) before
and after. Don't dispatch the nightly on a branch.

Public API: none. Wire: none.

## Related

- [#5099](https://github.com/moq-dev/moq/pull/5099) - the API change the shared client picked up
- [#4728](https://github.com/moq-dev/moq/pull/4728) - the released wire-compat harness and its planned-break tripwire
