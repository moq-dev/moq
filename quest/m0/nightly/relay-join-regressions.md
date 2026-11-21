# [M] The 2026-11-20 relay session regressions are fixed or accepted

## Goal

Nightly's Relay session benchmarks pass the paired comparison again: each
regression from 2026-11-20 is fixed, or accepted by the PR that caused it
with a stated reason.

## Plan

Found in run [44767045952](https://github.com/moq-dev/moq/actions/runs/44767045952),
`a759e4cf6` against `a4fa80a74`. Six regressions, consistent across all
three rounds, all on `moq-lite-07-wip`:

- `relay_session_join_broadcasts/*` +12 to 20%, at 1 to 8 relays and 64 or
  1024 broadcasts.
- `relay_session_join_relays/relays=8/publishers=16` +14.8%.
- `relay_session_delivery_paced/relays=1/publishers=16/viewers=256` +26.9%.

Suspects, the lite-07 route and subscription changes in that range:
[moq-dev/moq#9112](https://github.com/moq-dev/moq/pull/9112),
[moq-dev/moq#9155](https://github.com/moq-dev/moq/pull/9155),
[moq-dev/moq#9174](https://github.com/moq-dev/moq/pull/9174),
[moq-dev/moq#9179](https://github.com/moq-dev/moq/pull/9179),
[moq-dev/moq#9183](https://github.com/moq-dev/moq/pull/9183),
[moq-dev/moq#9202](https://github.com/moq-dev/moq/pull/9202), and
[moq-dev/moq#9219](https://github.com/moq-dev/moq/pull/9219). Bisect with a
paired local run of one `join_broadcasts` cell before changing code.

Public API: none expected. Wire: none.
