# [XS] moq-rtmp's rml has every upstream fix it needs

## Goal

moq-rtmp's vendored `rml` module carries every fix
[KallDrexx/rust-media-libs](https://github.com/KallDrexx/rust-media-libs) has
landed in rml_rtmp and rml_amf0 since the last check, each ported with its test
or recorded as not applying, and the module doc in
`rs/moq-rtmp/src/rml/mod.rs` names the newest upstream commit checked.

## Plan

Recurring child of the [vendor sync](/quest/m1/vendor-sync/README.md), which
says how each run works and recurs.

Last checked: rml_rtmp 0.8.0
([`a25df6bc`](https://github.com/KallDrexx/rust-media-libs/commit/a25df6bc35cdc33f21f7a35efc97e700dcc8fa17),
2023-04-29) and rml_amf0 0.3.0, vendored in
[moq-dev/moq#2060](https://github.com/moq-dev/moq/pull/2060). Upstream
`master` was at
[`953fc41d`](https://github.com/KallDrexx/rust-media-libs/commit/953fc41df2369d64e9c677369c706da797592e49)
(2023-05-31) on 2026-10-30, four commits later and dormant. The two that
matter, `write_all` with exact reads and lowercase publish modes, look already
present (`serializer.rs` and `sessions/server/mod.rs`); confirm and record
`953fc41d` as checked. Upstream being unmaintained makes this child a candidate
to drop in a later run.
