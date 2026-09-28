# [S] Plan TR 101 290 monitoring

## Goal

The TR 101 290 stream-health requirements in
[#1838](https://github.com/moq-dev/moq/issues/1838) become scoped
implementation quests with the open questions answered. This quest writes
quests, not code.

## Plan

Run `/quest-plan` against the issue and the current tree. The issue is a
requirements list (ETSI P1/P2/P3 checks, per-check counters, a per-stream
health roll-up) and asks for agreement before any skeleton; its parent
proposal #1799 is closed.

Settled by the issue, carry over:

- TS-level checks run at the TS edges (`moq import ts`, `moq export ts`,
  `moq-srt`), never in the media-agnostic relay.
- In today's media-aware lane our muxer regenerates PAT, PMT, PCR, and CC,
  so egress checks validate our own output and SI checks are not applicable.
  Full P1 to P3 conformance only means something for an opaque whole-mux
  lane, which nothing plans yet.
- Results surface through the existing stats plumbing (`moq-stats`), not a new
  transport. No remediation (FEC, 2022-7) and no GUI.

Questions the planning session must answer:

1. Where the checks live: a new crate, or beside the TS container in
   `rs/moq-mux/src/container/ts`.
2. The MVP subset: P1 plus the PCR, CC, and PTS parts of P2 is the issue's
   suggestion.
3. The configuration surface (thresholds, PIDs, sampling) and how the
   counters map onto `moq-stats` tracks.
4. Whether the opaque whole-mux lane is wanted at all; without it, P3 is out.

## Closes

- [#1838](https://github.com/moq-dev/moq/issues/1838) - close this issue when the quest finishes

## Related

- [TS import liveness](/quest/m1/3489-ts-import-stream-liveness.md) - the stream-stall case this model names `PID_error`
