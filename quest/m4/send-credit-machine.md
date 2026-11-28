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
  kio's [`lab/pure-machine`](https://github.com/kixel-ai/kio/tree/ce495aa3581085836b7aaf7e955c22d1b0f8c859/lab/pure-machine) SendCredit machine (left in
  the archived kio repository; start the port from it) reaches exactly TLC's state
  counts, 250 to 2,900 times faster than single-worker TLC.
- ✅ Keep `SendCredit.tla` as the design doc and run TLC only when the spec
  changes (rejected: retire TLC and delete the spec once the Rust machine
  matches). The spec stays the readable statement of the rules. `sh/tla.sh`
  already skips an exhaustive config whose spec, config, and TLC are unchanged,
  so Nightly's `credit` leg (about 35 minutes) needs no workflow change.
- ✅ The checker comes from kio's `model` feature (`rs/kio/src/model.rs`), a
  dev-dependency (rejected: a copy in moq-net, which every later consumer
  would copy again).

Guidance from kio#26's report:

- The machine holds only `Clone + Eq + Hash` state; waiters, `Weak` listings,
  and the data plane stay in the driver, which applies the machine's effects
  in order under the lock. The checker cannot see that boundary, so write its
  contracts down and test the driver against them.
- The spike's driver cost +266 instructions per event (+87%). Measure the
  port with the session benches; the cost is per credit event, not per frame.

Public API: none. Wire: none.

