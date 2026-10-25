# [S] Only sessions accept a request

## Goal

Answering an application claim's request with `accept` fails to compile in
Rust and to typecheck in TypeScript instead of panicking or throwing at
runtime. The peer-route hatches are crate-private.

## Plan

Decided 2026-10-21 (maintainer), after a downstream application's handler
called `request.accept(..)` on a `dynamic()` claim, which compiled and would
have panicked on the first request. Reshaped 2026-10-23 after the
public-model goal was dropped
([session-only items stay private](/quest/m1/api-audit/model-private.md)),
for [moq-dev/moq#6496](https://github.com/moq-dev/moq/pull/6496):

- `origin::Producer::dynamic_route`, `origin::Dynamic::update_route`, and
  `origin::Request::accept` become `pub(crate)` (JS `dynamicRoute`,
  `updateRoute`, and `Request.accept` are already `@internal`). An
  application answers with `prepare` then `publish`, or `reject`.
- Move the out-of-crate tests and benches that call them (moq-mux, moq-hls,
  moq-cli, moq-gst, moq-c, moq-ffi, moq-net integration, js/watch) onto
  `prepare`/`publish`.
- No `Forward`/`ForwardRequest` split: visibility already makes the call
  unreachable. Delete the runtime assert and its test; a compile-fail
  doctest and a `@ts-expect-error` line replace them.

Kept from [moq-dev/moq#5678](https://github.com/moq-dev/moq/pull/5678):
prepare and publish, one epoch per answer, and the cold-start reset until
[announce takeover](/quest/m1/announce-takeover.md) lands.

Parked 2026-10-24 (maintainer) until announce takeover lands: #6496's
conformance tests subscribe through a `dynamic()` claim and get `Unroutable`
from the cold-start reset (the worker's answer arrives under a fresh epoch),
not from a missing route; `dynamic()` already builds the same route as
`dynamic_route` minus `publishes: false`. Takeover removes the reset at its
source. On resuming, main has new out-of-crate callers to move: `wide_hop.rs`,
moq-hls `export/rendition.rs`, moq-cli `play/media.rs`, moq-mux
`benches/export.rs`, and moq-gst `source/imp.rs`, which used the removed
`Publisher::standalone` and needs a redesign.

Public API: moq-net breaking (three hidden items go private). Wire: none.

## Required

- [Announcement takeover](/quest/m1/announce-takeover.md) - removes the cold-start reset the claim's first subscribe hits
