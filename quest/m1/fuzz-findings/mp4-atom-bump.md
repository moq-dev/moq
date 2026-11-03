# [XS] moq-mux on a fixed mp4-atom

## Goal

moq-mux depends on an mp4-atom release with the `sgpd` entry-count bound
from [kixelated/mp4-atom#264](https://github.com/kixelated/mp4-atom/pull/264),
so the 22-byte init that grows a Vec past 3 GB in 0.17.0 fails to decode.
The `cmaf` and `fmp4` targets' `sgpd` and `iref` inputs are committed under
`rs/moq-mux/fuzz/regressions/`.

## Plan

#264 merged on 2026-10-30 as
kixelated/mp4-atom@06a532aabe47ea108343557fec4cf6c646c2a980, after 0.17.0
was published, so 0.17.0 lacks it.

moq-mux is already on 0.17.0
([moq-dev/moq#7308](https://github.com/moq-dev/moq/pull/7308)), which fixes
the `meta/iref.rs` subtract overflow (kixelated/mp4-atom#219), so what remains
is a move within 0.17 and the regression inputs. If the
[iloc fix](/quest/m1/fuzz-findings/mp4-atom-iloc.md) merges before the
release, the same bump covers it.

Public API: none expected in moq-mux. Wire: none.

## Waiting

- Maintainer: cut an mp4-atom 0.17.x release that contains kixelated/mp4-atom@06a532aabe47ea108343557fec4cf6c646c2a980 (agents do not cut it); check that crates.io's latest mp4-atom is above 0.17.0 and that its tag contains that commit
