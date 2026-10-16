# [L] Seeded moq-quic simulation

## Goal

The `Pair` harness in `rs/moq-quic/src/tests/util.rs` runs a deterministic,
impaired network: one seed reproduces a whole run, including loss,
duplication, reordering, and jitter in both directions. Property tests run
random workloads over it and assert that every byte arrives once and in
order, flow-control and stream limits hold, priority is honored, and the
connection closes cleanly. A failing case prints its seed.

The harness defaults to BBR3, the production controller, instead of Cubic.

## Plan

Decided 2026-10-16:

- **Determinism**: one seed flows into the CID generator, the reset key,
  token randomness, and BBR3's `probe_rng_seed` (which picks the ProbeBW
  wait and Reno-round bound), which today use `rand::rng()` and OS entropy. TLS key
  shares may stay nondeterministic if no observable behavior depends on
  them; otherwise use a deterministic test crypto provider. Ports stop
  depending on test order.
- **Impairments**: a seeded model in `drive_client` and `drive_server`,
  plus the bottleneck in the server-to-client direction, which today exists
  only client-to-server.
- **BBR3 default**: flip `cubic_transport()` in the harness to BBR3. A test
  that asserts Cubic-specific timing pins Cubic explicitly. A test that
  fails under BBR3 for a real bug stays on BBR3, `#[ignore]`d with a
  follow-up quest, rather than being hidden on Cubic.

## Related

- [Characterization tests](/quest/m1/quic/coverage/characterize.md) - targeted tests on today's harness, which can land before this
