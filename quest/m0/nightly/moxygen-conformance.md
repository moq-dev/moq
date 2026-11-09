# [M] Moxygen conformance cells pass again

## Goal

Nightly's `ietf-conformance / moxygen` job passes. Today its cells fail with
`Missing Begins With First Object Signal for group=0 subgroup=0`, and with
`Extension Error=2 ... Expected size: 1 Actual size: 0` in the Extensions 1/2
and Object/Group Counts 3/4 cells.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete from the triage of Nightly
runs [42377867523](https://github.com/moq-dev/moq/actions/runs/42377867523)
and [42487375545](https://github.com/moq-dev/moq/actions/runs/42487375545);
also red on 2026-11-06). ✅ Plan it (rejected: drop the lane), since it
keeps Nightly red, though moq-transport compatibility is low priority by the
repository's rules.

Find which side is wrong before changing anything: our relay dropping the
subgroup's first-object flag and an integer extension's value, or moxygen's
expectations diverging from the draft we implement. Fix ours when it is
ours. When moxygen is wrong, record it upstream and skip just those cells
with a link, rather than adding cost to match it.

Public API: none expected. Wire: moq-transport only, if ours is wrong.
