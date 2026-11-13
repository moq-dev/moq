# [S] Interop's go -> python media cell holds up

## Goal

The `go -> python` media cell of `just test interop --all` passes reliably,
fixed at its cause, never by a quarantine, a longer timeout, or a retry.

## Plan

Found 2026-11-12 in the Interop triage of scheduled run
[43260674067](https://github.com/moq-dev/moq/actions/runs/43260674067) (head
e9c6df852, the first run after #8455 and #8512). Its full-matrix step failed
`go -> python` with `audio gap after 648 samples` at 2 s of 30 s. It passed
once locally, and 8 more local runs of `--publishers go --subscribers
python,rust,go` passed every cell. No open PR names this cell.

Decided 2026-11-12 (maintainer): ✅ track it as this child of test flakes
(rejected: a new quest after a second red run; quarantining the cell).

Check first: [moq-dev/moq#8531](https://github.com/moq-dev/moq/pull/8531)
(`fix(hang,moq-mux): wait on a missing group for the delay, not the max`) merged
on 2026-11-12 and is in the failing run's head. A lead, not checked: it waits on
a missing group only for the playout delay, which is small at a cold start, so a
group that arrives out of order early on reads as an audio gap.

Reproduce with `just test interop --publishers go --subscribers python` and the
interop logs kept. Confirm or rule out #8531 first, then fix the cause.

Public API: none expected. Wire: none expected.
