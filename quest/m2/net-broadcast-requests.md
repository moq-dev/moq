# [S] @moq/net broadcasts expose their requests

## Goal

`@moq/net` exposes a public broadcast request handler (`requests()`
returning `Requests`, under Rust's name and shape) and exports the
broadcast's lister, so a JS application serves tracks it never created up
front from one broadcast, on demand, the way Rust does.

## Plan

Split 2026-11-03 (maintainer) from
[JS undeclared track names](/quest/m2/e2ee/js-undeclared-names.md), which
keeps the `@moq/crypto` half and now requires this. Kixel's T3 host
([kixelated/t3code#1](https://github.com/kixelated/t3code/pull/1)) needs
only the plaintext half: without it, one environment broadcast would need a
track per thread created up front, so T3 claims a broadcast per entity
instead and can collapse to one once this lands. Today the request pull
(`requested()` in `js/net/src/broadcast.ts`) is reachable only by wire
layers, and only the origin has a public `requests()`; keep the internal
wire path on the new handler. Ranked at the top of m2 beside the mesh
libraries, which Kixel builds on from `main`.
