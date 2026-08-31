# [S] Plan: the remaining iroh-live upstreaming

## Goal

A settled, per-PR split of [#2481](https://github.com/moq-dev/moq/issues/2481)
exists in the quest tree. Today the quest cannot land as one PR: most waves landed (renderer, playback, AEC) and the remainder is the hardware-gated Linux and embedded chain.

## Plan

Run /plan-quest with the maintainer: slim the plan around the surviving leaves (#2819, V4L2, MediaCodec) instead of executing the epic as written. The resulting quests replace
[the parent quest](/quest/m3/2481-merge-iroh-lives-native-media-stack-into-moq-video-moq.md) and carry its Closes entries forward.
