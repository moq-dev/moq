# [XS] js: every @moq package a package imports is a declared dependency

## Goal

No published `@moq/*` package imports another `@moq/*` package it does not
declare, and a check keeps it that way. `@moq/watch` on Firefox 140 ESR either
plays or fails with a mechanism named in #3361.

## Plan

`js/watch/src/broadcast.ts` imports `@moq/json` (`new
Json.Snapshot.Consumer(...)` in `#runCatalog`), but `js/watch/package.json`
does not declare it. `js/watch/vite.config.ts` externalizes every `@moq/*`
import, so the published bundle resolves it only through hoisting from
`@moq/hang`. A consumer whose resolver stubs it gets `Json.Snapshot` undefined
and exactly the reported `can't access property "Consumer", (void 0) is
undefined` inside an effect. The import predates 0.3.2, so it is not the
whole story: the only Firefox-relevant change between 0.3.2 and 0.5.2 is the
bowser user-agent gate (#2943, a new CJS runtime dependency in js/net), and
Firefox 140 takes the WebSocket path either way.

- Declare `@moq/json` in js/watch with the workspace range, and audit the other
  packages the same way.
- Add a check under `js/`, run by `just js check`, that every bare `@moq/*`
  specifier under a package's `src/` appears in that package's dependencies
  or peer dependencies. It fails on the current tree; fix what it finds.
- Reproduce on Firefox 140 ESR against `just relay` and `just pub bbb` with a
  source map. If the undeclared dependency was the mechanism, close; otherwise
  record the real stack in the issue and rescope.
- Firefox is not in the browser harness: add it to `test/wasm`'s Playwright
  matrix if the harness can drive it, or record why it cannot.

## Closes

- [#3361](https://github.com/moq-dev/moq/issues/3361) - close this issue when the quest finishes
