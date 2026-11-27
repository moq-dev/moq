# [M] Remove instance from hang::catalog

## Goal

Remove the optional `instance` field from `hang::catalog` renditions
(`video`, `audio`, `text`, `json`, `binary`) and `include` entries, along with
catalog-level instance pinning across `hang`, `moq-mux`, `moq-hls`,
`@moq/hang`, `@moq/watch`, bindings, and hang-04 docs. `moq-net`'s wire and
route `Instance` stays unchanged.

## Plan

Decided 2026-11-27 (maintainer, `/quest-plan`):

- ✅ Remove `instance` from `hang::catalog` completely (rejected: keep
  catalog-level `instance` pinning). Wire announcements already convey route
  `Instance` identity and consumers follow announcements directly; there is no
  remaining use case for pinning an instance inside `catalog.json`.
- ✅ Land in `m2` before the `0.22.0` cut (rejected: defer to `m3` after
  `0.22.0`). `0.22.0` freezes the hang-04 catalog JSON schema, so removing an
  unused field now avoids carrying it across a breaking format cycle.

Scope:

- Delete `pub instance: Option<moq_net::Instance>` from `VideoConfig`,
  `AudioConfig`, `TextConfig`, `JsonConfig`, `BinaryConfig`, and `Include` in
  `rs/hang/src/catalog/` and their JS equivalents in `js/hang/`.
- Remove catalog-side instance checks and pinning arguments in `rs/moq-mux`
  (`source.rs`, `catalog/producer.rs`, `json.rs`, etc.), `rs/moq-hls`
  (`export/rendition.rs`, `export/renditions.rs`, `export/include.rs`),
  `js/watch` (`broadcast.ts`, `include.ts`, `video/decoder.ts`, etc.), and any
  FFI or doc references to catalog `instance`.
- Keep `moq-net` route/wire `Instance`, `BroadcastConsumer::info().instance`,
  and announcement-driven restart/takeover behavior untouched.

## Related

- [Delete moq-pattern and @moq/pattern](/quest/m2/delete-moq-pattern.md) -
  companion pre-0.22 removal
