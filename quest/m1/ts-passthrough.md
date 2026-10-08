# [M] TS passthrough carries the multiplex verbatim on a fixed delay

## Goal

`moq import ts --passthrough` publishes a transport stream without
demultiplexing it, and `moq export ts --passthrough --delay <dur>` writes the
same bytes back out, paced on the source's own PCR and released at a fixed
delay like an SRT receiver. From the first group it releases, the output is
byte-identical to the input, so it is exactly as T-STD-conformant as the
source. It carries what the demultiplexed lane cannot: TS-level scrambling,
private PIDs, and PSI/SI as authored. There is no JS player for it.

## Plan

Decided (2026-10-01), from a discussion with t0ms:

- Why: the [T-STD line](/quest/m1/tstd/README.md) names a passthrough lane as
  the only one that can carry primary distribution until the remux passes, and
  none exists. Some feeds will want it permanently: the demultiplexed lane
  cannot parse a scrambled elementary stream into frames, and an operator
  who must hand on SI and private PIDs as authored has nothing to gain from
  a remux.
- The track is listed in a new `m2ts` root section of the hang catalog,
  not in `video`/`audio` (it has no codec to describe) and not in the
  `mpegts` section, whose members (`tracks`, `program`, `si`, ...) record
  what demultiplexing loses and are all in-band here. `m2ts` is one object,
  `{ track, randomAccess, muxRate }`, naming the broadcast's one passthrough
  track. Passthrough always carries the whole multiplex: splitting out a
  program would rewrite the PAT, so there is no scope field, and a second
  multiplex is a second broadcast. `muxRate` is present only while the
  source holds a constant rate, as in `mpegts`. 188-byte packets only; a
  192-byte source is refused, so there is no packet-size field. This quest
  specifies the section in `drafts/draft-lcurley-moq-mpegts.md` next to
  `mpegts`, with its mapping onto MSFTS's track fields. A track is one shape
  or the other.
- Objects are runs of whole 188-byte packets on one track. A group starts at
  a packet with `random_access_indicator` set on the program's video PID
  (its `stream_type` read from the PMT), or on the PCR PID when the program
  has no video, so a joiner lands on a decodable picture whichever PID
  carries the PCR. A group also starts at any PCR with
  `discontinuity_indicator` set, since MSFTS forbids a PCR discontinuity
  inside a group. A source that never sets the random-access indicator
  falls back to a group every N PCRs (N to be fixed in the PR, with a group
  spanning at most 1 s of PCR time). Pacing stays on the PCR PID throughout.
- A flagged PCR discontinuity that rewinds is a restart, as
  [TS restart](/quest/m0/broadcast-epoch/ts-restart.md) decides for the
  demultiplexed lane: the passthrough broadcast finishes cleanly and the rest
  of the input continues as a new broadcast at the same path under a fresh
  epoch, so object timestamps never go backwards within one broadcast. A
  flagged forward jump starts a group, and an unflagged rewind is fatal
  (decided 2026-10-07 in the final-head audit of #4670, which had a backward
  flag start a group in place).
- `randomAccess` is true only while the PAT lists a single program with at
  most one video PID, and every group has started at a
  `random_access_indicator`. The first group that starts without one (a
  fallback, or a `discontinuity_indicator` alone) republishes the catalog
  with it false. Any other layout always publishes false, since programs or
  video streams sharing a clock can stagger their GOPs.
  Both indicators sit in the adaptation field, which TS scrambling leaves in
  clear, so a scrambled feed still groups and paces.
- The publisher reads the PAT and PMT (with the existing `ts/psi.rs`
  parsers) to find the PCR and video PIDs and count programs, and parses
  nothing else past the adaptation field. PSI is never scrambled, and every
  byte is still published as received.
- Each object's timestamp is the PCR time of its first byte, interpolated
  between PCRs at the stream's own rate, carried in hang's `legacy`
  container (a varint timestamp before the packets) as verbatim tracks are.
  Object boundaries and timestamps then depend only on the bytes, so two
  publishers of one feed publish identical objects, which 1+1 needs.
- A multiplex is paced on one PCR PID: the `PCR_PID` in the PMT of the PAT's
  first program, or `--pcr-pid`. Every byte stays in order behind it. Programs
  on independent clocks are out of scope.
- The export reuses the [fixed-delay release](/quest/m1/tstd/delay.md)
  stage, keyed on each object's PCR time instead of a DTS, which also
  spreads each object's bytes at the PCR-implied rate. It must pace on the
  source's PCR, not on arrival: a pacer that re-clocks on arrival moves the
  PCR-to-PTS offset over a long capture and fails the decoder buffers, even
  though every byte is intact.
- An object that misses its deadline is dropped and counted, as the release
  stage does. The output shows a continuity error there; passthrough never
  rewrites continuity counters.
- Source clock drift is handled by the fixed-delay release's clock recovery
  (#4645), shared with the demultiplexed export, not here.
- Passthrough gives 1+1 identity for free: two exporters fed the same
  objects emit identical TS packets for every object both release,
  continuity counters included, since passthrough never rewrites them; an
  object one leg drops late is a gap in that leg only. Aligning the legs in
  time is left to the `--sync` anchor planned for the demultiplexed export's
  2022-7 legs, which passthrough can adopt.
  That is TS identity, not ST 2022-7 recovery, which also needs matching RTP
  headers from a coordinated RTP egress.
- Rust only. `js/hang` does not parse `m2ts`, and a player sees no
  rendition for the track.
- `--passthrough` publishes only the passthrough track. Publishing it
  alongside the demultiplexed tracks is out of scope.

Test: an export of a broadcast capture is byte-identical to the input from the
first released group, and the strict T-STD check gives the same verdict on
output and input. A scrambled fixture (`transport_scrambling_control` set on
its elementary PIDs) groups and paces the same as its clear twin. A
two-program fixture on one clock with staggered GOPs publishes `randomAccess`
false, as does one program with two staggered video PIDs. A single-program
fixture with the PCR on its audio PID starts groups at video random access
points only. PCR discovery finds a PCR PID that differs from the PMT PID, with
the PMT section split across packets. Two exporters fed the same objects with
different arrival skew emit identical bytes; when only one misses a deadline,
its output is the other's less that object's packets. A flagged backward PCR
discontinuity publishes two broadcasts, and the same rewind unflagged errors.
A dropped object is counted, and the rest still go out on time. Rerun the
#4613 netem rig (10% loss, 120 s) against it.

Update `doc/bin/cli.md` for both flags, `doc/concept` for the section, and
the draft's comparison section to say this repository now publishes both
shapes.

Public API: new import and export modes; nothing existing breaks. Wire: the
hang catalog gains an `m2ts` root section; additive.

## Required

- [TS restart](/quest/m0/broadcast-epoch/ts-restart.md) - the restart-under-a-new-epoch path a backward PCR discontinuity reuses
- [Fixed-delay release](/quest/m1/tstd/delay.md) - the release stage this reuses, with its clock recovery (#4645)

## Related

- [TS hitless](/quest/m2/ts-hitless.md) - the demultiplexed lane's 2022-7 legs and the `--sync` anchor
- [MSFTS convergence](/quest/m2/msfts-convergence.md) - the ES-level side of the same mapping
- [TS byte schedule](/quest/m1/tstd/byte-schedule.md) - the remux's equivalent of pacing on the source PCR
