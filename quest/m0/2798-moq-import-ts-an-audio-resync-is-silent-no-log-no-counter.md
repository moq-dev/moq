# [M] moq import ts: an audio resync leaves no trace

## Goal

A feed that is losing or substituting audio is distinguishable from a healthy
one: every completed resync logs a warning naming the PID and the bytes
discarded, and the importer exposes counters an operator can alarm on as a
rate.

## Plan

Since #2751 the MPEG-TS importer recovers from a damaged MP2, AC-3, or AAC
frame by scanning to the next confirmed sync instead of ending the session.
That was the right trade, but it took the diagnostic signal with it:
`Resync::recovered` in `rs/moq-mux/src/container/ts/import.rs` only zeroes its
counters, neither call site (`AacStream`, `LegacyStream`) logs, the importer
has no stats surface at all (its public API is `new`, `decode`, `seek`,
`finish`, `abort`), and the emitted timeline just steps 24 ms to 48 ms. Only
the budget-exhausted failure surfaces, as an error string. Testing against a
looped real broadcast also showed a frame published *unconfirmed* at a wrap
that lands inside a PES, which is a silent substitution rather than a gap, so
the counters have to cover any frame the demuxer published that it could not
confirm, not only the resync path.

- `tracing::warn!` on a completed resync that discarded bytes, carrying the PID,
  the track suffix, and the count.
- A `#[non_exhaustive]` `Stats` snapshot on `ts::Import` (resyncs, bytes
  discarded, frames published unconfirmed, per PID) and `moq import` logging it
  when it changes. A rate of resyncs is what is worth alarming on, so the counter
  matters more than any single line.
- Not in scope: bumping `container::Producer::discontinuity()` per resync. The
  TS importer never calls it today, and a marker group per lost 24 ms frame
  changes downstream behaviour; that is a separate decision.
- Tests: the one-byte-damage fixture from #2751 asserts one resync and its byte
  count in the snapshot; the looped-fixture wrap asserts the unconfirmed frame is
  counted.

## Closes

- [#2798](https://github.com/moq-dev/moq/issues/2798) - close this issue when the quest finishes
