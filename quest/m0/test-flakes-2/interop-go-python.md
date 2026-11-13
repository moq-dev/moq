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

Investigation 2026-11-13: #8531 does reach the Python audio decoder:
`moq_audio::decode::Consumer::next` calls `track.set_wait(self.delay())`.
This confirms the code path, not causation. The Go publisher stamps each
20 ms PCM batch on its contiguous sample clock, independently of its ticker.
The failing run's retained `go-python-audio.opus.txt` contains `48000 2`,
then timestamp/sample pairs `0/48`, `1000/120`, `3500/120`, `6000/120`,
`8500/120`, `11000/120`. These total 648 samples before the gap, one
20 ms batch minus the 312-sample Opus pre-skip. Its warning-level subscriber
log does not identify a skipped group or the gap's timestamp/duration.

On main at 65ca6b6ce, the focused command passed including finished-media
and sockets. A matrix attempt with `RUST_LOG=warn,moq_mux=debug` also passed
`go -> python` and `python -> cpp`, but failed overall: `tsc` was missing,
so all three JS clients were unavailable. This is not a complete-matrix
reproduction. No playback or client changes were made. #8611 was read first;
it currently changes only its own quest notes.

Next: restore the JS clients' TypeScript toolchain and repeat the complete
matrix with the same debug filter and retained logs until the gap reproduces.
Capture both the container skip reason and the audio gap timestamp/duration
before attributing the failure to #8531. Do not raise a budget on this evidence.
