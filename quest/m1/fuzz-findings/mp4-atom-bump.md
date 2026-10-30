# [S] moq-mux on a fixed mp4-atom

## Goal

moq-mux depends on an mp4-atom release with the `sgpd` entry-count bound
from [kixelated/mp4-atom#264](https://github.com/kixelated/mp4-atom/pull/264),
so the 22-byte init that grows a Vec past 3 GB in 0.16.2 fails to decode.
The `cmaf` and `fmp4` targets' `sgpd` and `iref` inputs are committed under
`rs/moq-mux/fuzz/regressions/`.

## Plan

Waiting on an outside action: #264 merging and a 0.17.x release of
mp4-atom (a maintainer cuts it; agents do not). Check with
`gh pr view 264 -R kixelated/mp4-atom` and the crates.io version; until both
hold, this quest stays ready and does nothing.

moq-mux pins `mp4-atom = "0.16.2"` (`rs/moq-mux/Cargo.toml`). 0.17.0 already
fixes the `meta/iref.rs` subtract overflow (kixelated/mp4-atom#219) and
carries breaking changes moq-mux must absorb: field-less `trun` runs stored as
a count (#254), unsigned version 0 `trun` composition offsets (#258), and the
ES descriptor's optional fields (#257). If the
[iloc fix](/quest/m1/fuzz-findings/mp4-atom-iloc.md) merges before the
release, the same bump covers it.

Public API: none expected in moq-mux. Wire: none.
