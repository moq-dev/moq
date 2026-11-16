# [S] moq-c reads each catalog's timestamp and takes a playout delay

## Goal

The C ABI reaches the same catalog timing the other SDKs have: a catalog
snapshot from `moq_consume_catalog` reports the media timestamp it takes
effect at, and the subscription takes the reader's playout delay, so a C
player behind the live edge reads in order every update it may still need.
Rust (`hang::catalog::Consumer::new(track, delay)`), JS
(`Catalog.watch(broadcast, { delay })`), and every moq-ffi binding
(`MoqMediaCatalogConsumer::subscribe(broadcast, delay)`, `MoqCatalog.timestamp`)
already do, from
[moq-dev/moq#8907](https://github.com/moq-dev/moq/pull/8907).

## Plan

- `rs/moq-c/src/consume.rs` reads through `moq_mux::catalog::hang::Consumer`,
  which drops the stamp and reads only the newest catalog. Read through
  `hang::catalog::Consumer::new(track, delay)` instead, as
  `rs/moq-ffi/src/media.rs` does, and keep each snapshot's `at`.
- Add `moq_consume_catalog_timestamp(catalog, out)` filling a
  `moq_timestamp`, and a delay argument, in the ABI's existing duration shape,
  to `moq_consume_catalog`.
- Settled by the maintainer (2026-11-16, [moq-dev/moq#8907](https://github.com/moq-dev/moq/pull/8907#issuecomment-6930706614)):
  change `moq_consume_catalog`'s signature in the next moq-c release (rejected:
  leaving moq-c on arrival timing).
- Mirror the Rust test `a_holding_reader_reads_every_update_within_its_delay`
  in `rs/moq-c/src/test.rs`, and update `doc/lib/c/index.md` and the C interop
  client.
