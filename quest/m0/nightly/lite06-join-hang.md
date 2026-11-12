# [XS] A scheduled Nightly's relay session job finishes inside its hour

## Goal

`Nightly (Relay session benchmarks)` (`just gh nightly rust-relay`) finishes
inside its hour with every cell, including the moq-lite-06 cells of
`relay_session_join_broadcasts` and `relay_session_join_stale`.

## Plan

The lite-06 join hang that kept those cells in their warm-up is fixed: a final
drop no longer hides a group the relay holds whole. Locally, every
`relay_session_join_*` cell finishes, the first lite-06 one in about 0.2 ms per
join. Confirm the first scheduled Nightly after the fix lands; never dispatch
Nightly.

## Waiting

- moq-dev/moq: the first scheduled Nightly on `main` after the fix lands finishes its `Relay session benchmarks` job inside the hour; check with `gh run list --workflow nightly.yml --event schedule`, then delete this quest
