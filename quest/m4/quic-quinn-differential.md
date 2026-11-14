# [S] Spike: moq-quic and quinn-proto run the same seeded workloads

## Goal

A test drives moq-quic and quinn-proto through the same seeded sim workloads
and reports every run where they disagree on an outcome both define: whether
the connection survives, which stream bytes arrive, and why it closed. The PR
states what it found on the existing seeds. If it finds nothing a simpler
check would not, it lands no test and records that.

## Plan

Decided 2026-11-13 (maintainer, /quest-plan on what testing to add):

- ✅ Keep it as a spike (rejected: dropping it, the agent's recommendation).
  The agent's case against: the fork has diverged (`RESET_STREAM_AT`, receive
  timestamps, probing, congestion control moved to moq-congestion, in-order
  reads only), so only coarse outcomes compare; it adds a quinn-proto
  dev-dependency; and [the interop runner](/quest/m1/quic-interop.md) already
  meets quinn as a peer. The spike measures whether that case holds.
- ✅ m4.

Agent's choices, for the implementer to revisit:

- Pin quinn-proto to the commit `rs/moq-quic/README.md` names as last
  checked, as a dev-dependency of a test-only crate so moq-quic's own tree
  does not grow.
- Restrict workloads to what both implement, and treat a disagreement as a
  question: a bug here, a bug upstream (report it), or a deliberate
  difference to record in the README's Upstream section.

Public API: none. Wire: none.
