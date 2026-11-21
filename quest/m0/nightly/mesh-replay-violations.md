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

The 6-relay rolls that hung (`roll f to new`, `roll b to old`) are still
unexplained: [moq-dev/moq#9213](https://github.com/moq-dev/moq/pull/9213) fixed
the drain that waited out its whole window, and a local ring6 run with 32
broadcasts on it passed with no hang, but `roll f to new` hung on the old
release build, which that drain change never reached. If a Nightly hangs a roll
again, reproduce it with the relay logs the run keeps.
Reproduce each with the replay's seed and fix in moq-net, one cause per
commit; if one turns out to be a known quest's case, add the entry instead.

Public API: none expected. Wire: none expected.
