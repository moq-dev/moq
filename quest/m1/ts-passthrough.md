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
  none exists. Some feeds will want it permanently: a scrambled multiplex
  cannot be demultiplexed, and an operator who must hand on SI and private
  PIDs as authored has nothing to gain from a remux.
- The track is listed in a new `m2ts` root section of the hang catalog,
  not in `video`/`audio` (it has no codec to describe) and not in the
  `mpegts` section, whose members (`tracks`, `program`, `si`, ...) record
  what demultiplexing loses and are all in-band here. `m2ts` maps a track
  name to `{ scope, randomAccess, muxRate }`: `scope` is `"program"` for a
  single program or `"multiplex"` otherwise, and `muxRate` is present only
  while the source holds a constant rate, as in `mpegts`. 188-byte packets
  only; a 192-byte source is refused, so there is no packet-size field. The
  section is specified in `drafts/draft-lcurley-moq-mpegts.md` next to
  `mpegts`, with its mapping onto MSFTS's track fields. A track is one shape
  or the other.
- Objects are runs of whole 188-byte packets on one track. A group starts at
  a packet on the PCR PID that carries a PCR with `random_access_indicator`
  set, and also at any PCR with `discontinuity_indicator` set, since MSFTS
  forbids a PCR discontinuity inside a group. A source that never sets the
  indicator falls back to a group every N PCRs (N to be fixed in the PR, with
  a group spanning at most 1 s of PCR time). `randomAccess` is true only
  while every group has started at an indicator; the first fallback group
  republishes the catalog with it false. All of these fields sit in the
  adaptation field, which TS scrambling leaves in clear, so a scrambled feed
  still groups and paces. Nothing past the adaptation field is parsed.
- Each object's timestamp is the PCR time of its first byte, interpolated
  between PCRs at the stream's own rate, carried in hang's `legacy`
  container (a varint timestamp before the packets) as verbatim tracks are.
  Object boundaries and timestamps then depend only on the bytes, so two
  publishers of one feed publish identical objects, which 1+1 needs.
- A multiplex is paced on one PCR PID: the first program's in the PAT, or
  `--pcr-pid`. Every byte stays in order behind it. Programs on independent
  clocks are out of scope.
- The export reuses the [fixed-delay release](/quest/m1/tstd/delay.md)
  stage, keyed on each object's PCR time instead of a DTS, and the CLI's
  `Delivery` pacer spreads each object's bytes at the PCR-implied rate. It
  must pace on the source's PCR, not on arrival: a pacer that re-clocks on
  arrival moves the PCR-to-PTS offset over a long capture and fails the
  decoder buffers, even though every byte is intact.
- An object that misses its deadline is dropped and counted, as the release
  stage does. The output shows a continuity error there; passthrough never
  rewrites continuity counters.
- Source clock drift is handled by the fixed-delay release's clock recovery
  (#4645), shared with the demultiplexed export, not here.
- Rust only. `js/hang` does not parse `m2ts`, and a player sees no
  rendition for the track.
- `--passthrough` publishes only the passthrough track. Publishing it
  alongside the demultiplexed tracks is out of scope.

Test: an export of a broadcast capture is byte-identical to the input from
the first released group, and the strict T-STD check gives the same verdict
on output and input. A scrambled fixture (`transport_scrambling_control` set
on its elementary PIDs) groups and paces the same as its clear twin. Two
exporters fed the same objects with different arrival skew emit identical
bytes. A dropped object is counted, and the rest still go out on time. Rerun
the #4613 netem rig (10% loss, 120 s) against it.

Update `doc/bin/cli.md` for both flags, `doc/concept` for the section, and
the draft's comparison section to say this repository now publishes both
shapes.

Public API: new import and export modes; nothing existing breaks. Wire: the
hang catalog gains an `m2ts` root section; additive.

## Required

- [Fixed-delay release](/quest/m1/tstd/delay.md) - the release stage this reuses, with its clock recovery (#4645)

## Related

- [MSFTS convergence](/quest/m2/msfts-convergence.md) - the ES-level side of the same mapping
- [TS byte schedule](/quest/m1/tstd/byte-schedule.md) - the remux's equivalent of pacing on the source PCR
