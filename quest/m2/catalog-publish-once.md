# [S] The catalog publish path compiles once

## Goal

`moq_mux::catalog::Producer<E>` and the `moq-json` diff serializer it runs over
`hang` catalog types codegen once, in `moq-mux`, instead of once per importer
crate (about 200 KiB each in `moq`, `moq_mux`, `moq_hls`, `moq_srt`,
`moq_rtc` and `moq_rtmp`; about 1 MiB of the CLI). Publishing a catalog costs
no more CPU than today, per `rs/moq-mux/benches/catalog.rs`.

## Plan

Found by the 2026-10 size audit. The diff in
`moq_json::snapshot` serializes straight off `T` against the previous
`serde_json::Value`, so its whole serializer is instantiated per `T`, and every
crate that calls a generic `Producer<E>` method re-instantiates it.

Options:

1. Run the diff serializer over `erased_serde::Serialize`, so it compiles once
   in `moq-json` and only `Catalog<E>::serialize` stays generic.
2. Serialize to a `serde_json::Value` once in `moq-mux` and diff values.
   Simplest, but adds a full tree build on every publish.
3. Non-generic entry points on `Producer<()>`, the type every importer uses.
   Leaves applications with an extension paying the copies.

Recommendation: 1, if the catalog benchmark holds; it fixes every `T`
`moq-json` serves, not just the catalog.
