# [S] Checks type-check the TypeScript under test/

## Goal

Every TypeScript harness under `test/` (such as `test/drain/drain.ts`) is
type-checked by `just check`, so a removed or renamed `@moq/*` API breaks the
PR that removes it instead of a nightly run.

## Plan

Found by [#5070](https://github.com/moq-dev/moq/pull/5070): `test/drain/drain.ts`
still called the deleted `writeString`, and only its review caught it. Today
`js/justfile`'s `check` runs each root workspace's `check` script.
`test/drain` is a root workspace but has no `check` script or tsconfig, so it
is skipped; `test/wasm` and `test/interop/clients/js-native` also lack a
`check` script. Standalone harnesses such as `test/max-age/client.ts` and
`test/interop/*.ts` sit outside any workspace tsconfig, and `sh/dispatch.sh`'s
`js` scope skips a change that touches only `test/drain/`.

- Type-check every `test/` TypeScript file, workspace or standalone, with
  `tsc --noEmit`.
- The scoped `just check` runs it when a harness or a `js/` package it imports
  changes.
- Prove coverage once with a deliberate type error in each kind of harness.
