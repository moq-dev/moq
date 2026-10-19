# [M] Audit decisions made to pass random-loss tests

## Goal

Every acceptance or performance decision a random-loss test drove (code,
defaults, harness choices, CI steps, and quests) is re-judged on the
bufferbloat harness and kept, reworked, or reverted, each with a recorded
verdict and its evidence.

## Plan

Decided 2026-10-17 (maintainer): random loss may prove correctness, but it
is not how real paths fail, so anything tuned to survive it as a quality or
performance target may be wrong. Scope is every random-loss-driven
decision, not only the 10% TS work. Small reverts land in the audit's PR;
anything bigger becomes its own quest. Verdicts are measured on the
bufferbloat harness (`just test ts --rate`, see `test/ts/README.md`), not
argued.

Measured with that harness 2026-10-18: BBRv3 alone keeps about a round trip
queued, so a deep queue never overflows and congestion lands at the relay as
`Old` group evictions instead. A verdict that needs congestion loss, such as
BBR's loss response, needs a queue about a round trip deep or a loss-based
sender sharing the path. The nightly overflowing path (2026-10-19) runs
CUBIC alone behind a 250 ms queue, which it overflows every run.

Leads found 2026-10-17 (no production default or congestion threshold was
changed for random loss; `LOSS_THRESH` is still 2%):

- `quest/m2/quic-bbr-natural-drain.md` targets "BBR's response to random
  loss" on a 10% profile and asks whether BBR should tell random loss from
  congestion. The main live risk: it would weaken BBR's loss response. Its
  premise came from #5805 moving 10% loss out of the T-STD epic.
- #4645's fixed-delay jitter buffer and skip design was partly justified by
  the 10% netem rig (#4613); re-check the skip budget and acquire wait.
- The BBR fork fixes (#4193, noq 1.3.1) were accepted on a 2% random-loss
  netem check; the fixes are correctness fixes, but re-check the evidence.
- Harness choices from #5805 and #5856: nightly `--hrd` because
  `--headroom` loses audio at 1% loss, WebSocket fallback disabled under
  `--loss`, a run that fails if the shaper dropped nothing, and
  `passthrough.py --loss-ok`.
- Decided 2026-10-17 (maintainer): the audit decides whether
  `quic-bbr-natural-drain.md` and `m3/quic-gcc.md` survive, since their
  premise rests on random loss.
- Other quests only list random loss among their measurement inputs:
  `quic-probe.md` (with noq#811 and noq#476), `quic-sim.md`,
  `quic-deadline.md` and `quic-careful-resume.md` ("the impaired path
  profile"), `m3/cut-through/bench.md`, and
  `m1/ts-reliability/recovery.md`'s network impairment arm. Their goals
  stand; the audit only moves any acceptance or performance input among
  them to bufferbloat.

Record each verdict (decision, origin PR, keep/rework/revert, evidence) in
the PR description.

Public API: depends on the verdicts. Wire: none expected.
