# [S] Plan: QUIC multipath for bonded contribution

## Goal

A settled, per-PR split of [#2276](https://github.com/moq-dev/moq/issues/2276)
exists in the quest tree. Today the quest cannot land as one PR: it spans a spike, an AsyncUdpSocket fan-out, and per-platform interface binding, several PRs at least.

## Plan

Run /plan-quest with the maintainer: split the step-1 spike from the fan-out socket and platform binding, and retarget paths to rs/moq-tokio. The resulting quests replace
[the parent quest](/quest/m2/2276-moq-native-enable-quic-multipath-for-bonded-contribution.md) and carry its Closes entries forward.
