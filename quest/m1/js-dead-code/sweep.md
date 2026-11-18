# [S] Remove the dead JS exports and members, and fix the manifests

## Goal

Every finding below is fixed and each package's `check` and `build` still
pass. Public API: none. Wire: none.

## Plan

Rule for exports: an export used only in its own file loses `export`; one
nothing uses is deleted. Re-run a grep for each name on the PR's head before
changing it.

Dead class members, never read anywhere:

- `LiveRanges.reportsStarts` (`js/net/src/lite/subscribe.ts`)
- `Id.canSend` (`js/qmux/src/stream.ts`)

Exports and types nothing outside their file imports (skif 0.9.0, 2026-11-17):

- js/net: `withJwt` (`connection/tokens.ts`), `Host` (`connection/endpoint.ts`),
  `FetchSubgroup` (`ietf/object.ts`), `MSG_PARAM_FORWARD` and
  `MSG_PARAM_SUBSCRIBER_PRIORITY` (`ietf/parameters.ts`), `ROUTE_MAX`
  (`lite/announce.ts`), `Kind`, `Claim`, `Carried` (`lite/rekey.ts`),
  `WireAnchor` (`lite/subscribe.ts`), `Listed` (`lite/tracks.ts`), `asIetf`
  (`stream.ts`), `isEmpty`, `rangesEnd`, `Containing` (`window.ts`)
- js/qmux: the `MaxStreamsBidi`, `MaxStreamsUni`, `DataBlocked`,
  `StreamDataBlocked`, `StreamsBlockedBidi`, `StreamsBlockedUni`, `Padding`,
  and `PingRequest` types (`frame.ts`). `RECOMMENDED_TRANSPORT_PARAMS` is
  used by `bench/datagrams.ts` and stays.
- js/hang: `Audio` (`catalog/audio.ts`), `Video` (`catalog/video.ts`), and the
  `Health` namespace types `Verdict`, `Report`, `Limits`, `Reset`, `Input`,
  `Evidence`, `Connection`, `Rendition` (`health.ts`)
- js/clock: `VALUE` and `NANOS` (`types.ts`). Exported classes use them as
  computed keys, so check `.d.ts` emit before un-exporting; keep them if
  declaration emit needs them.
- js/e2ee: `utf8`, `parse` (`derive.ts`)
- js/publish: `sleepUntil` (`source/pace.ts`)
- js/watch: `Speech` (`audio/replay.ts`), `hop` (`audio/stretch.ts`),
  `Window` (`sync.ts`)
- demo: `GameStatsSchema`, `GameStats` (`demo/boy/src/schemas.ts`),
  `createSetter` (`demo/boy/src/ui/solid.ts`), `kv` (`demo/web/src/viz.ts`)
- `test/` and `doc/concept/` hold about 50 more. Those found by the gate
  child's tool once it has every entry point are its to fix, not this
  quest's.

Manifests:

- Unused: `@moq/clock` and `playwright` in `test/stream-credit` (its
  `driver.ts` reaches Playwright only through
  `test/interop/clients/js/harness.ts`, which declares it), `zod` in
  `test/interop/clients/js-native`.
- Undeclared: `vue` in `doc` (`.vitepress/theme`), `playwright` in js/net,
  js/publish, and js/watch (their benches import it; pin the exact version
  the other packages pin, which `test/smoke/freshness.sh` ties to the
  toolchain), `vite` in js/hang (`src/vite-env.d.ts`), and `typescript` in
  `bench/browser`, `test/audio-quality/clients/js`,
  `test/interop/clients/js`, `test/interop/clients/js-native`, and
  `test/receive-credit`, whose `check` runs `tsc`.
- Drift: bring `demo/vr` from `vite` `^8.3.5` and `rolldown` `^1.2.16` to
  `^8.3.6` and `^1.3.0`, as the other workspaces take. It was created after
  the last dependabot group bump. `test/worklet`'s exact `vite` pin is
  deliberate (its manifest says why), so it stays. `test/smoke/clients/js`
  (`vite` `^8.1.5`) is outside the bun workspace and tests the published
  packages on its own pins, so it is no drift here.
- Sort the dependency blocks of `demo/web`, `doc/.vitepress/api`,
  `js/watch`, and `test/video`.

Not findings, despite what skif reported: the `@moq/*` dependencies of
`test/drain`, `test/receive-credit`, and the js-native client (their entry
points run from shell scripts), `@typescript/lib-dom` (TypeScript loads it
by name), `webpack`/`webpack-cli` in `test/worklet` (`Bun.resolveSync`), and
`remark-cli` (`sh/markdown.sh`).

## Related

- [js/common becomes a workspace](/quest/m1/js-dead-code/common-workspace.md) - declares the imports `js/common` makes, which this quest leaves to it
