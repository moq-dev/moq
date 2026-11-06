# [XS] moq-rtmp's rml has every upstream fix it needs

## Goal

moq-rtmp's vendored `rml` module carries every fix
[KallDrexx/rust-media-libs](https://github.com/KallDrexx/rust-media-libs) has
landed in rml_rtmp and rml_amf0 since the last check, each ported with its test
or recorded as not applying, and the module doc in
`rs/moq-rtmp/src/rml/mod.rs` names the newest upstream commit checked.

## Plan

Recurring child of the [vendor sync](/quest/m3/vendor-sync/README.md), which
says how each run works and recurs.

Last checked (2026-10-31): upstream `master` through
[`953fc41d`](https://github.com/KallDrexx/rust-media-libs/commit/953fc41df2369d64e9c677369c706da797592e49)
(2023-05-31), with every commit's disposition in the module doc and no
published advisories for rml_rtmp or rml_amf0. Upstream has been dormant since
that commit, so a run usually only re-checks commits and advisories. Kept
because RTMP parses untrusted network input and cargo-deny cannot see an
advisory against the vendored crates.

## Required

- [Cut and publish the prepared breaking release](/quest/m3/release/cut.md) - the next cycle starts after the cut
