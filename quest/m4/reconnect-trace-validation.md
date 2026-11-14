# [M] Reconnect runs are checked against the TLA+ model

## Goal

A seeded sweep of the client connect policy (`rs/moq-net/src/reconnect.rs`)
and the `moq_tokio::Connection` loop that drives it records every run, and
each is also a behaviour of `Handover.tla`: a change to handovers, probes, or
reported status that the model does not allow fails the sweep, without anyone
remembering to resync the model.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan on what could be proved about
moq-net): every TLA+ model gets trace validation, not route alone. Milestone
m4, behind route's.

- Reuse the trace format and the TLC recipe
  [route trace validation](/quest/m3/route-trace-validation.md) builds, with
  the `verify-reconnect` skill's Coverage table as the mapping from code to
  model actions.
- `reconnect/tests.rs` holds fixtures, not a sweep. Add a seeded one on the
  mocked clock that draws dial results, GOAWAYs, redirects, session ends, and
  probe outcomes, following the repository's seed convention. It is worth
  having without the model: check the skill's rules on each run directly too.
- The model also covers `moq-tokio/src/connection.rs`. Record that loop's
  steps as well, or say why the policy's trace alone is enough.
- The open-finding configs describe behaviour the code still has. A trace
  must be accepted by the spec those configs check, so a fix that removes a
  finding shows up here as well as in Nightly.

Public API: none. Wire: none.

## Required

- [Route sweeps are checked against the TLA+ model](/quest/m3/route-trace-validation.md) - the trace format and TLC recipe this reuses
