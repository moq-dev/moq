# [XS] moq-mux on a fixed mp4-atom

## Goal

moq-mux depends on an mp4-atom release with the `sgpd` entry-count bound
from [kixelated/mp4-atom#264](https://github.com/kixelated/mp4-atom/pull/264)
and the `iloc` zero-size extent bound from
[kixelated/mp4-atom#267](https://github.com/kixelated/mp4-atom/pull/267), so
the 22-byte init that grows a Vec past 3 GB and the 31 bytes of `iloc` that
allocate about 6 MB in 0.17.0 fail to decode. The `cmaf` and `fmp4` targets'
`sgpd`, `iref` and `iloc` inputs are committed under
`rs/moq-mux/fuzz/regressions/`.

## Plan

#264 merged on 2026-10-30 as
kixelated/mp4-atom@06a532aabe47ea108343557fec4cf6c646c2a980, after 0.17.0
was published, so 0.17.0 lacks it. #267 merged on 2026-11-03 as
kixelated/mp4-atom@2f1936206579b4c64d1d90538989246044e42ed6; the pending
release PR [kixelated/mp4-atom#260](https://github.com/kixelated/mp4-atom/pull/260)
picks up both.

moq-mux is already on 0.17.0
([moq-dev/moq#7308](https://github.com/moq-dev/moq/pull/7308)), which fixes
the `meta/iref.rs` subtract overflow (kixelated/mp4-atom#219), so what remains
is a move within 0.17 and the regression inputs. The `iloc` input is the
31-byte `cmaf` one from
[moq-dev/moq#7313's comment](https://github.com/moq-dev/moq/pull/7313#issuecomment-6520535160)
(`AB0AAAAcaWxvYwAAcwAAACAAyP////8AAAD///8AAA==`), committed as
`rs/moq-mux/fuzz/regressions/cmaf/mp4-atom-iloc`; on 0.17.0 the replay
reports 6,291,464 bytes allocated, and with #267 it passes. This quest's PR
also completes the [iloc quest](/quest/m1/fuzz-findings/mp4-atom-iloc.md) and
deletes it.

Public API: none expected in moq-mux. Wire: none.

## Waiting

- Maintainer: cut an mp4-atom 0.17.x release that contains kixelated/mp4-atom@06a532aabe47ea108343557fec4cf6c646c2a980 and kixelated/mp4-atom@2f1936206579b4c64d1d90538989246044e42ed6 (agents do not cut it); check that crates.io's latest mp4-atom is above 0.17.0 and that its tag contains both commits
