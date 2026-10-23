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

Public API: moq-net breaking (three hidden items go private). Wire: none.
