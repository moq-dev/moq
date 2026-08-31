# [S] Plan: the moq-cli capture and playback backlog

## Goal

A settled, per-PR split of [#2272](https://github.com/moq-dev/moq/issues/2272)
exists in the quest tree. Today the quest cannot land as one PR: it spans a dozen independent PRs (packaging, window/app capture, device enumeration, audio loopback, native playback).

## Plan

Run /plan-quest with the maintainer: split it; the packaging decision and moq devices are near-term, playback is its own line. The resulting quests replace
[the parent quest](/quest/m2/2272-moq-cli-remaining-work-for-capture-and-playback-window.md) and carry its Closes entries forward.
