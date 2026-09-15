# [S] json and binary config names agree across languages

## Goal

`Config` means the same thing in `@moq/json`, `moq-json`, `@moq/binary`, and
`moq-binary`. Today `@moq/json` uses `Config` for the codec options and
`ProducerConfig` / `ConsumerConfig` for the track-owning options, while
`moq_json::stream::ProducerConfig` still means the codec options, and
`ConsumerConfig` is declared twice with different shapes inside one JS
module (`js/json/src/stream/{decoder,consumer}.ts`, same for `snapshot`).
`moq-binary` carries the `ProducerConfig` compound and a
`with_compression(bool)` builder that the producer and consumer sides must
keep in sync by hand.

## Plan

Codec options are `Config` on both sides; the track-owning pair is
`producer::Config` / `consumer::Config` under public submodules in Rust
(`rs/moq-json/src/{snapshot,stream}`, `rs/moq-binary/src/{snapshot,stream}`)
and `Producer.Config` / `Consumer.Config` in JS, one declaration each.
Replace `with_compression(bool)` with a `compression: Compression` field
whose enum both sides share, so a mismatch is unrepresentable. Update
`doc/lib` for the four packages.

Public API: breaking on moq-json, @moq/json, moq-binary, and @moq/binary,
so on dev. Wire: none.

## Related

- [Merge dev](/quest/m1/merge-dev.md) - requires this so the published JSON packages do not carry two meanings of Config
- [JSON mutate](/quest/m2/json-mutate.md) - the additive twin of the same producer
