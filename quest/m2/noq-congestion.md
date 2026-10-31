# [M] noq adopts moq-congestion

## Goal

noq's BBR3 and Cubic run `moq-congestion`, its own copies are deleted, and
noq's public congestion API is unchanged. The PR merges in
[n0-computer/noq](https://github.com/n0-computer/noq) with its maintainers'
approval.

## Plan

Decided with the maintainer on 2026-10-23, answering the noq maintainers'
offer: moq owns BBR3 as an external crate rather than maintaining it inside
noq.

- Starts once the m1 cut publishes `moq-congestion` to crates.io (decided:
  no backport or manual publish). Check with `cargo search moq-congestion`.
- noq keeps its `Controller` trait public and stable; its `Bbr3`/`Cubic`
  configs become adapters that delegate to `moq-congestion`. noq does not
  re-export `moq-congestion` types, so each crate bump is an explicit noq
  change that wires any new signal.
- noq has no `SpaceId` and no `on_app_limited`; the adapter supplies what
  noq can and states what BBR3 loses without it. noq keeps its pacer and
  reads the controller's rate.
- Check against noq's open BBR3 reports before claiming them fixed:
  [n0-computer/noq#800](https://github.com/n0-computer/noq/issues/800),
  [n0-computer/noq#852](https://github.com/n0-computer/noq/issues/852), and
  [n0-computer/noq#819](https://github.com/n0-computer/noq/issues/819).
