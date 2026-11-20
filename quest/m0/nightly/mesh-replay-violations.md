# [M] The mesh replay's unknown violations are explained

## Goal

Nightly's 4- and 6-relay mesh replay jobs report no violation without a known
entry: each new one is fixed at its cause, or listed as known with the quest
that owns it.

## Plan

The replay jobs were added by
[moq-dev/moq#9153](https://github.com/moq-dev/moq/pull/9153) and first ran on
2026-11-19 (run [44562804083](https://github.com/moq-dev/moq/actions/runs/44562804083)). About 40 violations are
already listed as known and point at existing quests. These have no entry:

- ring of 4: `end a's broadcasts: shown x64`
- ring of 6: `reopen link a-b: unfed (stuck) x74 ... ended: internal error`

The 6-relay rolls that hung (`roll f to new`, `roll b to old`) belong to
[drain-exit](/quest/m0/nightly/drain-exit.md); re-check them once it lands.
Reproduce each with the replay's seed and fix in moq-net, one cause per
commit; if one turns out to be a known quest's case, add the entry instead.

Public API: none expected. Wire: none expected.
