# [L] moq-net's send credit is a pure state machine checked in cargo test

## Goal

moq-net's send credit (`rs/moq-net/src/transport/open.rs` and its cursor)
becomes a pure `StateMachine` that the I/O code drives. Its `cargo test` runs
kio's exhaustive BFS checker over the machine, checks `SendCredit.tla`'s
invariants, and asserts TLC's distinct-state count on all four configs in
`rs/moq-net/verify/credit/` (`SendCredit`, `SendCreditPriority`,
`SendCreditStreams`, `SendCreditLive`), so the code is its own model.

## Plan

Decided 2026-11-27 (maintainer, /quest-plan on kio's follow-ups). Milestone m4:
verification tooling nobody waits on.

- ✅ Port it (rejected: drop it, which would also drop kio's model-check
  quest). [kixel-ai/kio#26](https://github.com/kixel-ai/kio/pull/26) showed
  kio's `lab/pure-machine` SendCredit machine reaches exactly TLC's state
  counts, 250 to 2,900 times faster than single-worker TLC.
- ✅ Keep `SendCredit.tla` as the design doc and run TLC only when the spec
  changes (rejected: retire TLC and delete the spec once the Rust machine
  matches). The spec stays the readable statement of the rules. `sh/tla.sh`
  already skips an exhaustive config whose spec, config, and TLC are unchanged,
  so Nightly's `credit` leg (about 35 minutes) needs no workflow change.
- ✅ The checker comes from kio's `model` feature, a dev-dependency
  ([kixel-ai/kio `quest/m2/model-check.md`](https://github.com/kixel-ai/kio/blob/main/quest/m2/model-check.md);
  rejected: a copy in moq-net, which every later consumer would copy again).
  Bumping moq's kio dependency to a release or revision with it is part of
  this quest.

Guidance from kio#26's report:

- The machine holds only `Clone + Eq + Hash` state; waiters, `Weak` listings,
  and the data plane stay in the driver, which applies the machine's effects
  in order under the lock. The checker cannot see that boundary, so write its
  contracts down and test the driver against them.
- The spike's driver cost +266 instructions per event (+87%). Measure the
  port with the session benches; the cost is per credit event, not per frame.

Public API: none. Wire: none.

## Waiting

- kixel-ai/kio: the `model` feature lands on kio's `main` (`quest/m2/model-check.md`, which starts once this quest exists). Check: kio's `Cargo.toml` has a `model` feature.

## Required

- [kio main](/quest/m0/kio-main.md) - moq runs on kio's `main`, so this quest's bump only moves past the `model` feature

## Related

- [Send credit reset wake](/quest/m4/send-credit-reset-wake.md) - settles `MakeRoom`'s reset wake in the spec this machine must match
