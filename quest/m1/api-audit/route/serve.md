# [M] Serve a held broadcast

## Goal

An application answering a prefix route's request can hand over a broadcast
it already holds: `request.serve(consumer)` in Rust and
`request.serve(broadcast)` in JS, mirrored in moq-ffi, moq-c and the
wrappers. The answer is served under the prefix route's own identity and
announces no exact route. A client's first subscribe through it succeeds
over moq-transport-18, moq-transport-22, moq-lite-06 and moq-lite-07-wip.

Non-goals: the route handle rename ([One Route handle](/quest/m1/api-audit/route/handle.md)
carries `serve` over), and removing the reset for answers that announce a
per-path route ([announce takeover](/quest/m1/announce-takeover.md)).

## Plan

Decided 2026-10-30 (maintainer); see the [epic](/quest/m1/api-audit/route/README.md).
This lands first, on today's `origin::Requests`/`Request`, so moq.pro's
sync-ingest mirror (`rs/edge/src/sync/ingest.rs`, which calls
`request.accept`) and the external relay operator have a supported answer
before the larger rework.

- Make the crate-private `Request::accept` (`rs/moq-net/src/model/origin.rs`)
  public as `serve`, for application claims as well as sessions. Replace the
  `compile_fail,E0624` doctest with usage docs. In JS, expose `#accept` as
  `serve` and drop `hooks.acceptRequest` if nothing else needs it.
- Contract, in rustdoc and `doc/concept/identity.md`: a served answer is
  part of the prefix route's stream at that path. Today that means its
  instance; after [Identity and epoch](/quest/m1/api-audit/route/identity.md),
  its identity, which promises one continuous sequence space per path. To
  serve different bytes at a path that was already served, start a new
  prefix route rather than serving new bytes under the old one. A job that
  may restart per path answers with `prepare`/`publish`, and takes the
  cold-start reset until [announce takeover](/quest/m1/announce-takeover.md).
- Served-answer cache: an answer that `prepare`/`publish` stored in the
  prefix route's served cache must not serve a later request once its
  exact route is withdrawn. Check `ServeState`'s cache on withdrawal, fix
  it if the answer survives, and test it.
- Tests: turn the reproduction from planning into an integration test in
  `rs/moq-net-tests/tests/integration/`. A server origin `requests("")`
  linked over `transport::memory`; the handler serves a held broadcast; the
  client's first `request` plus `subscribe` reads a frame on every version
  above. Keep the prepare/publish case asserting today's reset, so
  takeover's change to it is visible. Mirror it in `js/net`.
- `MIGRATE.md` (rs/moq-net, js/net): mention `serve` beside the existing
  #6496 entry. It is additive.

Public API: additive `Request::serve` in Rust, JS and the bindings. Wire:
none.

## Related

- [moq-dev/moq#6496](https://github.com/moq-dev/moq/pull/6496) - made `accept` crate-private; this reopens it under a stated contract
