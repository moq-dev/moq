# [M] Sans-IO session benchmark in moq-net

## Goal

A Criterion bench in `moq-net` measures a full publisher to relay to
subscribers path with no sockets: a publishing client, relay sessions that
forward through an origin the way `moq-relay` does, and N subscribing
clients, all over the in-memory transport. It sweeps subscribers and frame
size, over lite and IETF, so a per-subscriber or per-frame cost shows as a
slope and CI can compare it with little noise.

## Plan

`rs/moq-net/tests/support/{mock,harness}.rs` already pairs sessions over an
in-memory WebTransport mock and runs the full handshake. Reuse it from the
bench, as `moq-uring`'s benches reuse their test support, rather than
exporting a mock from the crate.

- Keep setup (handshakes, announce, the subscribe round trip) outside the
  timed region. Time delivery of groups until every subscriber has received
  every frame, and count delivered bytes so a skipped delivery can't look like
  a speedup.
- Also time subscriber join, since a relay pays that per viewer.
- Report throughput in bytes and frames.
- Pick sweep points that finish quickly enough to run on every `moq-net` PR.

## Related

- [Relay session bench](/quest/m1/bench-relay.md) - the same shape through the real `moq-relay` handler
- [Benchmark regressions in CI](/quest/m1/bench-ci.md) - tracks this bench on PRs and nightly
