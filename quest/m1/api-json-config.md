# [S] One Config per JSON mode, in both languages, with no dead field

## Goal

A JSON or binary track is configured by one type per mode whose
`compression` is the `Compression` enum, in Rust and TypeScript alike, and
the catalog entry that describes it carries no field nobody writes.

## Plan

- `moq_json::window` and `@moq/json` Window take `Config { op_ratio,
  compression: Compression, checkpoint_records }` and
  `consumer::Config { compression }`, the shape #3718 gave snapshot and
  stream; today they keep `ProducerConfig`/`ConsumerConfig` with a
  `compression: bool`, and the TS side declares `ConsumerConfig` twice with
  two shapes. Drop `snapshot::Config::with_delta_ratio`, the last builder.
  Call sites: `rs/moq-mux/src/timeline.rs`, `rs/moq-room/src/chat.rs`,
  `js/hang/src/container/timeline.ts`, `js/room/src/chat.ts`.
- `moq_mux::json::Config` and `moq_mux::binary::Config` (`compression: bool`
  plus `with_compression`) go; `catalog::Producer::json_snapshot(track,
  entry: JsonConfig)` and its siblings take the hang catalog entry directly
  and refuse an entry whose `mode` disagrees. The moq-ffi `bool` follows,
  and with it `rs/libmoq`, the five hand-written wrappers, and `doc/lib/*`
  per the Cross-Package Sync checklist, in this quest and not after the
  merge.
- Delete the per-track `timeline` field on `JsonConfig` and `BinaryConfig`
  in `rs/hang`, `js/hang`, and the hang draft; the timeline lives under the
  root `archive` since #3612 and nothing writes the field.
- Add `Mode::Window` to the catalog data sections and the draft; moq_json
  ships three modes and the timeline is itself a window track.

Public API: breaking on moq-json, @moq/json, moq-mux, hang, @moq/hang, and
moq-ffi, so on dev. Wire: the hang catalog drops a never-written field and
gains a mode value; update `drafts/draft-lcurley-moq-hang.md` in the same
PR and run `just drafts check`.

## Related

- [JSON mutate](/quest/m2/json-mutate.md) - the closure edit that lands on the same producers
- [JSON merge](/quest/m2/json-merge.md) - the consumer side
