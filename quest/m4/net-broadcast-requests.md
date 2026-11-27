# [M] @moq/net broadcasts expose their requests

## Goal

`@moq/net` exposes a public broadcast request handler (`requests()`
returning `Requests`, under Rust's name and shape) and exports the
broadcast's lister, so a JS application serves tracks it never created up
front from one broadcast, on demand, the way Rust does.

## Plan

Split 2026-11-03 (maintainer) from
[JS undeclared track names](/quest/m5/e2ee/js-undeclared-names.md), which
keeps the `@moq/e2ee` half and now requires this. Kixel's T3 host needs
only the plaintext half: without it, one environment broadcast would need a
track per thread created up front, so T3 claims a broadcast per entity
instead and can collapse to one once this lands. Today the request pull
(`requested()` in `js/net/src/broadcast.ts`) is reachable only by wire
layers, and only the origin has a public `requests()`; keep the internal
wire path on the new handler.

`Track.Request`, which a handler hands out, is `@internal` until then
([moq-dev/moq#8929](https://github.com/moq-dev/moq/pull/8929)): drop the tag
when `requests()` publishes it.

Moved to m4 2026-11-12 (maintainer, /quest-audit): the downstream host it was ranked for is parked.

Ending an on-demand track comes with it: in Rust, dropping the last producer
of a track nobody wants ends it cleanly, and one a reader joined in the race
goes back to the broadcast's handler as a fresh request
([moq-dev/moq#9550](https://github.com/moq-dev/moq/pull/9550)). JS
`Producer.close()` still aborts an unfinished track with `Cancel`; give it the
Rust drop's behavior once a JS handler can serve the re-request.
