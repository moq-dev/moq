# [S] TS stats module

## Goal

Every moq-mux TS stats type lives in the `container::ts::stats`
module beside `stats::Log`: `ts::Stats` becomes `ts::stats::Snapshot` and
`ts::StreamStats` becomes `ts::stats::Stream`. `StreamStats.track` becomes an
owned `String`, and `ts::MultipleProgramsError` becomes `#[non_exhaustive]`.
No new fields and no behavior change.

## Plan

Decided while planning the follow-ups of
[#4505](https://github.com/moq-dev/moq/pull/4505) and
[#4506](https://github.com/moq-dev/moq/pull/4506):

- **Breaking.** `Stats` and `StreamStats` have been published since
  moq-mux 0.9.14. #4506 kept them at `ts::` on `release` for that reason.
- **Names: `stats::Snapshot` and `stats::Stream`.** `Snapshot` matches
  `moq_net::stats::Snapshot`. `stats::Import` was rejected because it reads
  like `ts::Import`, and `stats::Pid` because the row is a stream's liveness,
  not the PID itself.
- **`StreamStats.track` becomes `String`.** This is batched here so there is
  one break, not two. [TS health stats](/quest/m2/ts-health-stats.md) needs
  it so the type deserializes.
- **`MultipleProgramsError` gets `#[non_exhaustive]`.** Callers keep
  recovering it by downcast (`moq-cli` hints `--program` from it) and keep
  reading `programs`. It can gain fields without another break. A `ts::Error`
  enum was rejected: the importer returns `anyhow`, so callers would still
  downcast.
- Update moq-cli's `publish.rs`, the only consumer that names these types.
  moq-srt only uses `stats::Log`.
- Main still adds fields under the old names: #4584 added `crc_error`, and
  [TS import health](/quest/m2/ts-import-health.md) adds more. `Export::stats`
  (#4577) also returns the old `ts::Stats`. The rename carries those at merge time. Update the type names in the quests still open
  when this lands, including [media stats schema](/quest/m1/stats/schema.md)
  and [Rust reporters](/quest/m1/stats/rust.md).

Public API: breaking renames in moq-mux. Wire: none.
