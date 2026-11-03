# [S] JS follow returns a Following

## Goal

In `@moq/net`, `Origin.Consumer.follow(path, ...)` returns its own
`Following` type, matching Rust and every binding, and `request` keeps
`Requesting`. Each type documents one meaning. js/watch's annotation
(`js/watch/src/broadcast.ts:284`) and the other callers compile against it. js/watch's cache
entry (`js/watch/src/broadcast.ts:172`) and the local at `:285` hold either
result, so they take a shared read-side type (the common `active`/`closed`
shape) or `Requesting | Following`.

## Plan

Found by the 2026-10-30 origin API audit
([route epic](/quest/m1/api-audit/route/README.md)). Today both methods
return `Requesting`, whose docs (`js/net/src/origin.ts:1624-1650`) describe
both behaviors. Keep the `active`/`closed` shape, and rename only where a
member means something different for `follow`. A local helper in
`js/net/src/connection/forward.ts:121` is also named `follow` but
re-receives a route; rename it so it stops reading as `Consumer.follow`.

Public API: breaking for TypeScript type annotations only. Wire: none.
