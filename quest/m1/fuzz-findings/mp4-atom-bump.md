# [S] moq-mux on a fixed mp4-atom

## Goal

moq-mux depends on an mp4-atom release with the `sgpd` entry-count bound
from [kixelated/mp4-atom#264](https://github.com/kixelated/mp4-atom/pull/264),
so the 22-byte init that grows a Vec past 3 GB in 0.16.2 fails to decode.
The `cmaf` and `fmp4` targets' `sgpd` and `iref` inputs are committed under
`rs/moq-mux/fuzz/regressions/`.

## Plan

Waiting on an outside action: a 0.17.x release of mp4-atom that contains
#264 (a maintainer cuts it; agents do not). #264 merged on 2026-10-30 as
kixelated/mp4-atom@06a532aabe47ea108343557fec4cf6c646c2a980, after 0.17.0
was published, so 0.17.0 lacks it. Check the crates.io version and that its
tag contains that commit. Until it does, this quest stays ready and does
nothing.

moq-mux pins `mp4-atom = "0.16.2"` (`rs/moq-mux/Cargo.toml`). 0.17.0 already
fixes the `meta/iref.rs` subtract overflow (kixelated/mp4-atom#219) and
carries breaking changes moq-mux must absorb: field-less `trun` runs stored as
a count (#254), unsigned version 0 `trun` composition offsets (#258), and the
ES descriptor's optional fields (#257). If the
[iloc fix](/quest/m1/fuzz-findings/mp4-atom-iloc.md) merges before the
release, the same bump covers it.

Public API: none expected in moq-mux. Wire: none.
