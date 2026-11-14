# [XS] A scheduled Nightly's relay session job finishes inside its hour

## Goal

`Nightly (Relay session benchmarks)` (`just gh nightly rust-relay`) finishes
inside its hour with every cell, including the moq-lite-06 cells of
`relay_session_join_broadcasts` and `relay_session_join_stale`.

## Plan

The lite-06 join hang itself is fixed: a final drop no longer hides a group the
relay holds whole. The first scheduled Nightly after that fix still timed out
because its paired base, the previous Nightly's commit, predated the fix and hung
in the same cell; the base run now ends when it prints nothing for ten minutes and
only marks its target unavailable. Confirm the first scheduled Nightly after that
lands; never dispatch Nightly.

## Waiting

- moq-dev/moq: the first scheduled Nightly on `main` after the idle bound lands finishes its `Relay session benchmarks` job inside the hour; check with `gh run list --workflow nightly.yml --event schedule`, then delete this quest
