# [L] A route's role is its type

## Goal

No route method panics, throws, or returns an error because the route was
made by the other constructor. Today one `Route` type serves both exact
routes (`origin.route(path)`) and prefix routes (`route_prefix`):
`Route::broadcast()` panics on a prefix route in Rust
(`rs/moq-net/src/model/origin.rs`, "# Panics"), throws in JS
(`js/net/src/origin.ts`), and returns `MoqError::Internal` in moq-ffi
(`rs/moq-ffi/src/origin.rs`), while `requested()` ends at once on an exact
route. The fix lands in Rust, JS, moq-ffi, the hand-written C ABI, and every
binding wrapper, and `RouteInfo`'s public `Default` (whose `instance()`
panics) goes private.

## Plan

Decided 2026-11-07 (maintainer, /quest-plan from the API shape audit):
replace [moq-dev/moq#7806](https://github.com/moq-dev/moq/pull/7806)'s
single handle with a shape where the misuse is unrepresentable. ✅ Prototype
two shapes and pick from the prototypes (rejected: an `Option`-returning
`broadcast()`, which keeps a runtime check in every caller; keeping #7806 as
is). "Every route is a prefix on the wire" (maintainer), so:

- **Single prefix Route**: one `Route` always claims a prefix.
  `route.broadcast(suffix)` publishes a broadcast under it, an exact route is
  the empty suffix, and `requested()` yields requests for any suffix the
  route covers.
- **Split**: `route()` returns `Route` (with `broadcast()`), `route_prefix()`
  returns a `Claim` (with `requested()`), and the shared methods (`announce`,
  `withdraw`, `set_cost`, `set_auth`, `instance`) are on both.

Build both in Rust first, port each to the in-repo callers (moq-mux, hang,
moq-cli, moq-relay, moq-archive, moq-ffi, moq-c) and sketch the diff for
downstream callers (`.route(` and `route_prefix` sites), then
put the comparison in the PR under `## Waiting` with a recommendation. After
the maintainer picks, land the winner in JS, moq-ffi, moq-c, and the
wrappers in the same PR. The route instance accessor's five spellings across
bindings stay with [the exporter](/quest/m3/ffi-exporter.md).

## Required

- [A job's exact route continues its claim's readers](/quest/m2/announce-takeover.md) - changes how `Request::route` lends a claim's instance, which both shapes must keep
