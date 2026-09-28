# [S] Remove moq-wasm

## Goal

`rs/moq-wasm`, `js/wasm`, `just wasm`, and the WASM path in `test/wasm` are
deleted: the browser runs moq-net as generated TypeScript instead.

## Plan

Decided in planning: keep the experiment until generated lite ships, then
delete it rather than polish it. Remove its entries from the size report,
the justfiles, the wasm clippy lane (keep `moq-net` and `moq-mux` there if
anything still targets wasm32), and the docs, and update the P2P questline's
note about `moq-wasm`.

Public API: removes the unpublished `@moq/wasm` package. Wire: none.

## Required

- [Generated lite](/quest/m1/rs2ts/lite.md) - the replacement
