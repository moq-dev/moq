# [S] Addressing by time guide

## Goal

A documentation page explains how applications address groups after the
timestamp cut: seeking and starting from a time, why group starts strictly
increase, how a group ends (whole, incomplete, or aborted), how a restart
mints a new epoch instead of continuing numbers, and what group IDs
lite-01..06 and moq-transport peers see. Publishers and player
authors can follow it without reading the drafts.

## Plan

Decided 2026-10-25 (maintainer): a separate page, not only inline doc
updates. Write it against the design [the model](/quest/m1/time-addressing/model.md)
and [lite-07](/quest/m1/time-addressing/lite07.md) quests record, including
that a released relay fills a hole with one FETCH per ID, so a mixed-version
deployment upgrades its relays together. Link it from
`doc/concept/moq-lite.md` and from the identity and recovery guide
(`doc/concept/identity.md`), which covers epochs and restarts.

## Required

- [Lite-07 by start](/quest/m1/time-addressing/lite07.md) - the wire this page describes
