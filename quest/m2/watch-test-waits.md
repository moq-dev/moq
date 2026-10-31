# [S] Watch tests wait for the change they expect

## Goal

`js/watch` tests wait for the signal or state they assert on instead of a
fixed number of event-loop yields, so they neither flake under load nor pass
before the change happens. A scope error from an origin request or follow is
caught once in `Broadcast.#requestBroadcast` and reported, so every caller
(decoders, text renderer, catalog filter) gets it without its own handling.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7312](https://github.com/moq-dev/moq/pull/7312):

- Fixed yields in watch tests:
  - ✅ Quest in m2, one quest across `js/watch` tests. Reason: a fixed
    yield count is flaky by construction.
  - Drop.

Waits: many test files define their own `settle`/`flush` as N
`setTimeout(0)` or microtask turns (`js/watch/src/broadcast.test.ts:58`,
`video/decoder.test.ts:15`, `video/source.test.ts:9`, `audio/emitter.test.ts:50`,
`media.test.ts`, `sync.test.ts`, `audio/config.test.ts`, ...). Add one shared
test helper that waits on a `Getter` until a predicate holds, with a
bounded timeout that fails loudly, like `waitFor` in
`js/net/src/integration.test.ts:1702` and `js/net/src/auth.test.ts:41`, and
move the tests onto it. Keep a plain yield only where the test asserts that
nothing changed, and say so there.

Scope error: `#requestBroadcast` (`js/watch/src/broadcast.ts:247`) calls
`origin.follow(path)`/`origin.request(path)`, which throw for a path outside
the origin's scope; `#runBroadcast` catches the same throw into `out.error`,
and [moq-dev/moq#7312](https://github.com/moq-dev/moq/pull/7312) (open) catches it again in its contribution branch. Catch it once
in `#requestBroadcast`, return `undefined`, and report it as the other
refusals are, then drop the per-caller catch. The error stays with the
rendition that asked, not the broadcast's root `out.error`: test a catalog
with one out-of-scope rendition beside valid ones and assert the valid ones
still play. Merge main after [moq-dev/moq#7312](https://github.com/moq-dev/moq/pull/7312) if it lands
first.

Public API: none. Wire: none.
