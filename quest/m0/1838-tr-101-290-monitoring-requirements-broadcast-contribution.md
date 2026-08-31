# [M] TR 101 290 monitoring: requirements (broadcast/contribution health metrics)

## Goal

Implement and verify the behavior tracked in [#1838](https://github.com/moq-dev/moq/issues/1838)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Goal

For MoQ to replace satellite/contribution links and hand off to IRDs, it needs the
stream-health telemetry operators expect from SRT / Zixi / RIST. ETSI TR 101 290 is the
industry yardstick for MPEG-TS integrity. This issue specifies *what* to monitor and
*where* to surface it, so we can agree scope before building a skeleton. Part of #1799.
No implementation here, requirements only.

#### Where monitoring runs (design question)

The relay core is media-agnostic by design, so TS-level monitoring belongs at the **TS
edges**, not in the relay:

- **Ingest** (e.g. `moq-srt`, `moq-cli publish ts`): validate the incoming contribution
  feed and expose its health.
- **Egress** (e.g. `moq-srt` m=request, `moq-cli subscribe --format ts`): validate the
  TS we hand downstream.
  Important nuance from the two-lane model (#1799):
- **Media-aware lane** (today): PAT/PMT/PCR/CC are *regenerated* by our muxer, so at
  egress TR 101 290 mostly validates *our own output* (still valuable, catches muxer
  regressions like the DTS issue #1836). SI tables beyond PAT/PMT don't exist in this
  lane, so those checks are N/A.
- **Opaque whole-mux lane** (future, Option B): the original PCR cadence, CC, and SI
  survive, so TR 101 290 validates the *real* contribution feed faithfully. This is where
  full P1/P2/P3 conformance is meaningful.
  Proposed: implement the checks once over a TS byte/packet stream, run them at both edges,
  and surface results through the existing stats plumbing (cf. #1783 connection stats,
  \#1671 per-track stats; `moq-net`/`moq-relay` stats modules).

#### Checks to implement (configurable thresholds; ETSI defaults)

##### Priority 1 (loss of these = not decodable)

- TS\_sync\_loss (loss of sync after N consecutive bad sync bytes; default 5)
- Sync\_byte\_error (sync byte != 0x47)
- PAT\_error / PAT\_error\_2 (PAT absent, repetition > 0.5 s, PID 0 wrong table\_id, scrambled)
- Continuity\_count\_error (CC discontinuity / wrong increment / illegal duplicate)
- PMT\_error / PMT\_error\_2 (PMT repetition > 0.5 s, scrambled)
- PID\_error (a referenced PID not seen within a user-defined window)

##### Priority 2 (recommended continuous monitoring)

- Transport\_error (TEI bit set)
- CRC\_error (PAT/PMT/CAT/NIT/EIT/BAT/SDT CRC)
- PCR\_repetition\_error (PCR interval > 40 ms)
- PCR\_discontinuity\_indicator\_error (> 100 ms jump w/o discontinuity\_indicator)
- PCR\_accuracy\_error (PCR drift outside +/- 500 ns)
- PTS\_error (PTS repetition interval > 700 ms)
- CAT\_error (CAT required when scrambling present)

##### Priority 3 (application dependent; mostly opaque-lane only)

- NIT/SDT/EIT/TDT/RST\_error, SI\_repetition\_error, unreferenced\_PID

#### Surfacing

- Per-check counters + last-event timestamp, per PID where applicable.
- Aggregate per-stream "health" suitable for an operator dashboard (green/amber/red per
  priority, akin to an SRT/Zixi stats panel).
- Exposed through the existing stats API so relay/CLI consumers can read it; no new
  bespoke transport.

#### Out of scope (for now)

- Remediation (FEC, 2022-7) - separate egress work.
- Full DVB SI semantic validation beyond presence/repetition/CRC.
- A GUI; this is the metrics source, not a dashboard.

#### Open questions for discussion

1. Does monitoring live in a dedicated `moq-ts`/`moq-monitor` crate, or inside the
   ingest/egress crates that already touch TS?
2. Which subset is MVP, P1 + the PCR/CC/PTS parts of P2?
3. Configuration surface (thresholds, which PIDs, sampling) - CLI flags vs config file.
   A private reference implementation of these checks exists and can inform thresholds and
   edge cases; happy to share it as background (not as a code drop).

## Closes

- [#1838](https://github.com/moq-dev/moq/issues/1838) - close this issue when the quest finishes
