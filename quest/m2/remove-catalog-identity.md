# [M] Remove identity from hang::catalog

## Goal

Remove the optional `identity` field from `hang::catalog` renditions
(`video`, `audio`, `text`, `json`, `binary`) and `include` entries, along with
catalog-level identity pinning across `hang`, `moq-mux`, `moq-hls`,
`@moq/hang`, `@moq/watch`, bindings (`moq-ffi`, `libmoq`), and hang-04 docs.
`moq-net`'s wire and route `Identity` type and
`BroadcastConsumer::info().identity` stay unchanged.

## Plan

Decided 2026-11-27 (maintainer, `/quest-plan`):

- ✅ Remove `identity` from `hang::catalog` completely (rejected: keep
  catalog-level `identity` pinning). Wire announcements already convey route
  `Identity` and consumers follow announcements directly; there is no remaining
  use case for pinning an identity inside `catalog.json`.
- ✅ Land in `m2` before the `0.22.0` cut (rejected: defer to `m3` after
  `0.22.0`). `0.22.0` freezes the hang-04 catalog JSON schema, so removing an
  unused field now avoids carrying it across a breaking format cycle.

Scope:

- Delete `pub identity: Option<moq_net::Identity>` from `VideoConfig`,
  `AudioConfig`, `TextConfig`, `JsonConfig`, `BinaryConfig`, and `Include` in
  `rs/hang/src/catalog/` and their JS equivalents in `js/hang/`.
- Remove catalog-side identity checks and pinning arguments in `rs/moq-mux`
  (`source.rs`, `catalog/producer.rs`, `json.rs`, etc.), `rs/moq-hls`
  (`export/rendition.rs`, `export/renditions.rs`, `export/include.rs`),
  `js/watch` (`broadcast.ts`, `include.ts`, `video/decoder.ts`, etc.),
  `rs/libmoq`, `rs/moq-ffi`, and the hang-04 draft/docs.
- Explicitly preserve `moq_net::Identity` on the wire and route table,
  `BroadcastConsumer::info().identity`, and announcement-driven
  restart/takeover behavior.
