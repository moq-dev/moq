# [M] Every moq-quic sim run is checked against QUIC's rules

## Goal

Each run of moq-quic's seeded sim (`tests::sim`, the fuzz `connection` target,
and `just rs quic-sim`) fails when either endpoint breaks a rule of RFC 9000,
9001, or 9002 that an observer can check, not only when data fails to arrive.
A regression that still delivers every byte, such as overshooting the
congestion window or the amplification limit, fails the seed that shows it.

## Plan

Decided 2026-11-13 (maintainer, /quest-plan on what testing to add): the sim asserts stream and datagram integrity, so a sender that breaks a
rule and still delivers passes. `qlog` is compiled and never read. A checker
turns every existing seed into a conformance run at no extra search cost.

- ✅ m1 (rejected: m4; all new testing quests in m1).
- ✅ No RFC requirement map in the Duvet style (rejected in the same
  interview): it shows a requirement is cited, not that it holds. The checker
  is the list of rules that are enforced.

Agent's choices, for the implementer to revisit:

- An observer in `tests::util::Pair` that sees every datagram and each
  endpoint's exposed state, independent of the code it judges. Prefer rules
  read off the wire (a server sends an unvalidated address at most three
  times what it received, client Initials are padded to 1200 bytes, nothing
  but a close after closing, no datagram over the path MTU, which
  [the MTU quest](/quest/m4/quic-sim-mtu.md) adds) over rules that trust the
  endpoint's own counters (bytes in flight within the window, stream and
  connection flow control never exceeded by either side, stream counts within
  the peer's limit, PTO backoff doubling, idle timeout honoured).
- Each rule ships with a patch that breaks it and proof the checker catches
  it, kept the way `test/drill/mutations` keeps its patches. A rule with no
  such patch does not land.
- List the rules considered and left out, with the reason.
- The 64-case gate run must not slow measurably; say by how much it did.
- The relay and moq-tokio's `Client` and `Server` already run over moq-tokio's
  `quic::driver::sim::Network` (`rs/moq-relay/tests/integration/quic_sim.rs`,
  `rs/moq-tokio/tests/integration/sim_client.rs`), whose paths cross the
  shared `Link`, so a checker there judges those runs too.

Public API: none. Wire: none.

## Related

- [QUIC Interop Runner](/quest/m1/quic-interop.md) - the same rules judged by other stacks
