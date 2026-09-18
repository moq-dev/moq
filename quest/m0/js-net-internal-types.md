# [XS] @moq/net publishes the types its Connection props name

## Goal

The published `@moq/net` declaration files resolve. `ReloadDelay` and
`ReloadStatus` in `js/net/src/connection/reload.ts` are tagged `@internal`,
and `js/tsconfig.json` sets `stripInternal`, so the emitted `reload.d.ts` is
`export {}` while `pool.d.ts` still imports both for `ConnectionProps.delay`,
`Connection.status`, `Connection.Delay`, and `Connection.Status`. A consumer
of the built package gets unresolved types on the one class everyone uses.

## Plan

Drop the two `@internal` tags (the `Reload` class keeps its own). Prove it
with a test that runs `tsc --emitDeclarationOnly` on `js/net` and fails on
any `.d.ts` that imports a name the target file does not export, so the
next `@internal` on a referenced type cannot ship. Public API: none.
Wire: none.

## Related

- [@moq/net API](/quest/m1/api-js-net.md) - the shape review that touches the same files
