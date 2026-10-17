# [M] moq-net follows announced instances

## Goal

moq-net offers the Rust equivalent of JS `request(path, { announced: true })`
(`js/net/src/origin.ts`): one request that follows each publisher instance
serving a path, unless an epoch is pinned. moq-ffi's
`MoqAnnouncedBroadcast::available()` (`rs/moq-ffi/src/origin.rs`) and moq-c's
`moq_origin_announced_broadcast` (`rs/moq-c/src/origin.rs`) use it and drop
their hand-rolled follow loops.

## Plan

Decided with the maintainer on 2026-10-17: the goal as drafted, from
decision 5 of [#5700](https://github.com/moq-dev/moq/pull/5700), which kept
the loops per binding until moq-net owns them. Mirror the JS name.

The sink follow helper waits on this quest so it does not add a third loop;
the OBS source keeps calling `available()`, whose API does not change.

The loops #5700 added drain `origin::Consumer::follow`, resolve the path with
`routed_broadcast`, and settle three cases moq-net should own once:

- An `End` forgets the last instance, so the same instance returning after
  a gap is handed out again.
- The last instance is held weakly, so a handle the caller let go of never
  keeps its instance from retiring.
- A change that arrives while a resolution is in flight, such as an end and
  a start for the instance it resolved, resolves afresh; and a resolution
  that runs ahead of its held `Restart` is not handed out twice.

Two JS behaviors have no Rust counterpart to mirror. JS falls back to blind
resolution while a `consume` session lacks discovery; moq-net has no blind
requests. Decided with the maintainer on 2026-10-17: no blind fallback. The
mode waits on an announced route as `routed_broadcast` does, and blind
requests stay out of scope, so it differs from JS when discovery is
unavailable. JS also watches the path so a `consume` session asks its peer about
it. A moq-net session asks for its whole granted scope at setup today
(`interest_prefixes` in `rs/moq-net/src/lite/subscriber.rs`), so this quest
registers no interest. [Rust lazy discovery](/quest/m1/lazy-discovery/rust.md)
narrows that to watched prefixes, lands after this quest, and owns making
this mode watch its path, as JS does.

JS returns a handle whose current broadcast changes per instance
(`Request.active`); Rust's `request_broadcast` takes only an optional epoch
today. Propose the Rust shape in the PR. Keep #5700's binding tests passing
unchanged as the regression, and add moq-net tests for the three cases plus
a pinned epoch: an announced request that pins epoch A refuses a replacement
announced with epoch B instead of following it, mirroring JS `an
epoch-pinned announced request refuses another instance`
(`js/net/src/origin-request.test.ts`).
Public API: a new moq-net request mode or handle, and simpler binding
internals with no binding API change. Wire: none.
