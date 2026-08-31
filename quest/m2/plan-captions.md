# [S] Plan: caption and subtitle text tracks

## Goal

A settled, per-PR split of [#2280](https://github.com/moq-dev/moq/issues/2280)
exists in the quest tree. Today the quest cannot land as one PR: it spans catalog schema, cue format choice, importers, and player UI.

## Plan

Run /plan-quest with the maintainer: fix the cue format and catalog shape before the implementation PRs; reconcile with the text rendition contract at /quest/m2/text-schema.md. The resulting quests replace
[the parent quest](/quest/m2/2280-hang-caption-and-subtitle-text-tracks.md) and carry its Closes entries forward.
