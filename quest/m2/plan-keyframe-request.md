# [S] Plan: the keyframe request message

## Goal

A settled, per-PR split of [#2284](https://github.com/moq-dev/moq/issues/2284)
exists in the quest tree. Today the quest cannot land as one PR: it is a moq-lite wire change gated on an unanswered scoping question.

## Plan

Run /plan-quest with the maintainer: answer whether relay-cache fetch_group already covers the common case, then settle the generic-vs-PLI-specific message design. The resulting quests replace
[the parent quest](/quest/m2/2284-moq-net-keyframe-request-pli-equivalent-for-fast-tune-in.md) and carry its Closes entries forward.
