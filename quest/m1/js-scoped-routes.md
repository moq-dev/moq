# [S] JS scoped readers pick from their own routes

## Goal

A scoped `@moq/net` origin reader, and a connection publishing that scoped
view, announces a prefix whenever some route there overlaps its scope, and
presents the preferred route among those, as moq-net does. For example, with
a cheap `*/chat` route and a costlier `*/video` route both at `""`, a
`room/video` reader sees the video route instead of nothing.

The cost of `forwardAnnounced` fanning one advertisement out across interest
streams is measured, and fixed only if the sweep shows it matters.

## Plan

Correctness first. `OriginState.snapshot()` in `js/net/src/origin.ts` keeps
one `preferredEntry` per path before `#listed` applies the reader's scope,
and `rebuildOriginated()` makes the same early choice, so a reader whose
scope excludes the globally preferred entry drops the path even though
`bestEntry` would route a request through another one. moq-net filters each
entry by the cursor's scope (`RouteEntry::overlaps` in
`rs/moq-net/src/model/origin.rs`) before choosing. Keep the candidates until
the scope applies. The shared unscoped snapshot is the fast path the
`js/net/bench/broadcasts.ts` sweep guards; keep it, and run that sweep before
and after. Raised by Codex on https://github.com/moq-dev/moq/pull/4234.

Then the fan-out. `forwardAnnounced` in `js/net/src/connection/forward.ts`
opens one announce stream per interest head and keeps a `Dynamic` plus a
`drive` task per stream, so a peer advertising a prefix above several heads
(`""` for `a/**` and `b/**`) is stored and repriced once per head, and closing
one stream can drop the entry a request was using while an identical one
remains. Extend the nightly JS benchmark with a sweep over heads and routes
(the repo's fan-out rule), then decide: if cost grows with heads times routes
at realistic sizes, share or reference-count received prefixes across
streams; if not, record the numbers in the PR and leave it.

No public API or wire change is expected.
