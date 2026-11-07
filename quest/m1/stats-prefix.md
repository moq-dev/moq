# [XS] Only the relay's own stats publisher uses the stats prefix

## Goal

No client session can publish or announce under the relay's stats prefix (`.stats` by default), whatever its grant. Today `Cluster::publisher` scopes purely from the grant's patterns, while the stats registry excludes the whole `<prefix>/**` subtree from metering, so a root `**` grant can publish unmetered media there and replace a node's `.stats/node/<node>` broadcast that aggregators read.

## Plan

Found by the 2026-11-07 invariant audit (relay-1). Cut the prefix out of non-peer publish scopes in `Cluster::mounted`/`scope` (`rs/moq-relay/src/cluster.rs`); peers still relay each other's stats. Test that a `**` publisher cannot announce under the prefix and that subscribers still can read it.
