# [L] Redundant TS exports render common content identically

## Goal

Two `moq export ts --sync` legs of one broadcast, joined at different
moments and fed with different delivery skew, emit identical TS for every
slot whose content both release, continuity counters included, from their
first common slot, within one epoch. A slot whose content one leg lacks
keeps its position in that leg as stuffing, so complementary losses stay
complementary.

This is TS identity only. RTP packetization for ST 2022-7 is
[its own quest](/quest/m2/ts-rtp-egress.md), and merging the legs is the
receiver's job.

Non-goal: identity across a replaced broadcast. A leg ends on a replacement, or
starts a fresh stream with `--stitch`,
so legs may differ after one (#5101, maintainer 2026-10-09: byte-identical
output across restarts is not supported).

## Plan

Decided 2026-10-13 by the maintainer: the coordinate the
legs share is the broadcast's media timeline, each object's timestamp on its
catalog clock, with a constant mux rate. Two exports of one broadcast
already read the same objects and timestamps, so this needs no importer
alignment; [same-epoch importers](/quest/m2/hop-aligned-import.md) extend it
to legs fed by different importers of one epoch. Every byte a `--sync` leg
renders from the objects it has (slot, PCR, table, PES header, continuity
counter) is a function of those objects, that coordinate, the catalog, and
the rate, never of what the leg has sent or when it joined. 2022-7 is a differentiator for primary distribution, not
a requirement (t0ms's review of #4645), so this waits until the T-STD line
settles.

- Release is unchanged: measured acquisition and the fixed-delay release's
  clock recovery. This supersedes the 2026-10-01 plan to release at
  `clock.wall + pts + delay`. The catalog's wall mapping is fixed for the
  epoch, so a wall-scheduled release spends its delay on the source's clock
  error (a 1 s delay lasts about 9 h at 30 ppm), and NTP on the hosts does
  not change that. The legs' differential delay stays their delivery skew,
  which the receiver absorbs.
- Continuity counters restart at each group. Each media PID's first packet
  in a group sets `discontinuity_indicator` and is numbered from there. The
  PCR moves to its own PID, because the flag on the PCR PID would declare a
  time-base break. Loss is still detected within a group.
- Content one leg misses never changes what the other renders, and the leg
  that misses it renders stuffing in its slots. Forcing both legs to drop the
  same content would defeat redundant paths.
- A leg that lacks an access unit cannot know how many packets it fills, so
  no later packet's slot may depend on an earlier access unit's size. A
  constant rate fixes capacity, not placement. Settle the placement rule
  before building the scheduler: fixed per-PID reservations on the timeline,
  a packet plan carried with the objects, or each group re-anchored at a
  slot derived from its timestamp, with headroom.
- t0ms's
  `two_legs_render_a_skip_the_same_way`
  (`rs/moq-mux/src/container/ts/export_timing_test.rs`) becomes this
  complementary case and stops being ignored. With the audio late, it also
  showed a PES header byte that differs at the skip; find and fix that.
- `--sync` refuses what it cannot keep: an output with no constant rate
  (neither `--mux-rate` nor the catalog's recorded rate), since a measured
  rate gives each leg its own grid; `--stitch`, since identity is per epoch;
  and `--passthrough`, whose output is already the source's packets.

Tests on mocked time compare slots keyed by their media time, not by output
position, and assert continuity counters too (`assert_same_packets` stops
masking them under `--sync`):

- legs joined at different source offsets, mid-group and a group apart;
- different delivery skew and read cadence, each track lagging in turn;
- complementary loss, each leg missing groups the other has;
- a broadcast timeline crossing the 33-bit PCR and PTS wrap while both run,
  and a leg joining just after it;
- ±30 ppm source drift;
- each refusal.

The validation work (#5328) found the `just test ts --pair` arm at 86.96%
SDT agreement over 41 seconds of clean delivery, below its 90% gate. Its
grader associates a table with the next PES PTS; check that association
against the slot scheduler before attributing mismatches to table emission.
Do not loosen the threshold, and do not claim packet identity from table
agreement alone.

Public API: `moq export ts --sync`.

## Required

- [T-STD TS export](/quest/m1/tstd/README.md) - the export, clock recovery, and schedule these legs share

## Related

- [TS over RTP legs](/quest/m2/ts-rtp-egress.md) - packetizes these slots with matching RTP headers for ST 2022-7
- [Same-epoch importers](/quest/m2/hop-aligned-import.md) - extends the coordinate to legs fed by different importers
- [TS passthrough export](/quest/m2/ts-passthrough-export.md) - identical packets for common objects by construction, in its own lane
