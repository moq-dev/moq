# [L] Request handlers are named requests

## Goal

The handle that answers incoming requests is named for its role at every
layer: `origin`, `broadcast`, and `track` producers have `requests()`
returning `Requests`, which yields `Request`s, in Rust, JS, and the bindings.

## Plan

Decided 2026-10-23 (API audit follow-up, maintainer): `dynamic()` names the
mechanism; `auth::Handle::requests() -> auth::Requests` is the precedent.

- Rename `{origin,broadcast,track}::Producer::dynamic()` (and
  `track::Request::dynamic()`) to `requests()`, and each `Dynamic` type to
  `Requests`. JS `dynamic()`/`Dynamic` mirror it. moq-ffi, its wrappers,
  and moq-c (`moq_origin_dynamic*`) follow, since moq-c takes the same breaks
  (decided 2026-10-25).
- Rename the crate-private peer-route hatches (`dynamic_route`,
  `update_route`) with the rest.
- Quests that say `Dynamic` (subscribe ranges, archive) switch
  to the new name in this PR.

About 240 Rust, 30 binding, and 10 JS call sites; land it in one sweep.

Public API: moq-net, `@moq/net`, moq-ffi, wrappers, and moq-c renamed. Wire: none.
