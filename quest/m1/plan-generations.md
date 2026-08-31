# [S] Plan: historical broadcast generations

## Goal

A settled, per-PR split of [#2873](https://github.com/moq-dev/moq/issues/2873)
exists in the quest tree. Today the quest cannot land as one PR: its discovery contract is partly contradicted by dev's announce rework (the Ended flag was removed; recordings are discovered out of band).

## Plan

Run /plan-quest with the maintainer: re-plan the wire and API design against the current dev model and the broadcast epoch quest at /quest/m2/broadcast-epoch.md. The resulting quests replace
[the parent quest](/quest/m1/2873-moq-net-enumerate-and-resolve-historical-broadcast.md) and carry its Closes entries forward.
