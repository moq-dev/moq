# [M] Acceptance and performance run on bufferbloat, not random loss

## Goal

Every acceptance or performance (QoS) check that impairs a network does it
with a rate limit below the media bitrate behind a deep but finite queue, so
it congests and tail-drops like a real path. Random loss remains only where
a test proves correctness, such as retransmits happening.

## Plan

Decided 2026-10-17 (maintainer), and recorded in the root `AGENTS.md`:
bufferbloat does double duty, since its overflow drops packets too, so it is
the impairment for anything judged on quality, latency, or throughput.
Random loss judged that way pushed decisions toward surviving a failure mode
real paths do not show (see the [audit](/quest/m1/random-loss-audit.md)).

Classify each random-loss user, move the acceptance and performance ones to
shaping, and leave correctness checks alone:

- TS harness: `just test ts` gains `--rate` and `--queue` beside `--loss`
  (decided 2026-10-17), passed to `moq-shaper`'s existing token bucket; its
  100 ms default queue is shallow for bufferbloat. The nightly
  `--hrd --loss 1` and `--passthrough --loss 1` steps in `ts.yml`,
  `passthrough.py --loss-ok`, and the `test/ts/README.md` lossy-path section
  grade output quality, so they move to `--rate`/`--queue`; `--loss` stays
  for correctness runs.
- Relay drills: the 5% profile every `::impaired` lane and the cluster
  drills use (`drills.rs`, `test/drill/README.md`). Decide per drill
  whether it proves correctness or judges behavior under impairment.
- `moq-quic`'s sim (`Link.loss`, the lossy proptests) and BBR3's
  deterministic periodic-drop unit tests prove recovery is correct; they
  stay unless they assert throughput or latency.

`moq-shaper --loss` stays for correctness use. Earlier in the same day the
plan was to delete it; the maintainer then scoped the rule to acceptance and
performance.

The TS run's oracle under a path narrower than the muxrate, decided
2026-10-17: the export stays T-STD conformant and continuous (PCR,
continuity counters, no partial PES), sheds whole groups rather than
stalling, and end-to-end latency stays bounded instead of growing with the
queue. No loss-percentage or hole-count thresholds. Pick the shaped rate and
queue depth from what the run must exercise (the queue must overflow), and
say why in the harness.

Public API: none. Wire: none.

## Related

- [Random-loss audit](/quest/m1/random-loss-audit.md) - re-judges what random-loss tests drove, on this harness
- [Relay burst drill](/quest/m1/test-flakes-2/relay-burst-impaired.md) - owns the impaired burst drill's flake
- [TS recovery](/quest/m1/ts-reliability/recovery.md) - MoQ content-loss schedules, separate from network impairment
