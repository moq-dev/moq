# [M] TS passthrough export writes the multiplex back byte-identical on a fixed delay

## Goal

`moq export ts --passthrough --delay <dur>` reads the track a broadcast's
`m2ts` catalog section names and writes the same bytes back out, paced on the
source's own PCR and released at a fixed delay like an SRT receiver. From the
first group it releases, the output is byte-identical to the input, less any
object dropped late. On a complete path, validate preservation of the source
schedule as well as the bytes. Missing bytes can break a decoder's acquisition
or buffers; packet identity alone does not preserve T-STD conformance under loss.
There is no JS player for it.

## Plan

For raw fallback, reuse the passthrough quest's packet/position and schedule
oracle. Extend its fault coverage to compare surviving packets against their exact positions
in the admitted source interval, including null packets, tables, unknown
PIDs and adaptation bytes. Assert preserved gaps in the release schedule,
no rewriting or duplication, and observed loss counters. A dropped object
may contain part of a PES or the next acquisition information; record when
decode is unavailable rather than claiming an unchanged-byte proof also
proves decoder conformance. Whole MPTS coverage includes programs with
unaligned recovery points and clocks.

Decided (2026-10-01), from a discussion with t0ms, and split from the TS
passthrough quest on #5003, whose import half it completes:

Confirmed in the 2026-10-11 audit: carry either one supplied SPTS or the whole
MPTS unchanged. Do not extract programs or split PIDs in this mode. Separate
SPTS inputs have separate raw broadcasts/tracks under the existing one-multiplex
catalog contract; extracting a program from an MPTS changes
the packet sequence and is not the byte-identical fallback. This matches the
distinction between `unmodified-program`, `unmodified-multiplex` and
`per-program` in [MSFTS](https://datatracker.ietf.org/doc/html/draft-gregoire-moq-msfts).

The exactness boundary is the admitted source interval, starting at the first
released group. Account for the importer's current acquisition prefix and
188-byte framing restrictions explicitly; do not call this whole-file identity.
Test group acquisition without inserting synthetic tables or configuration
into the output. A periodic group boundary or one program's RAI does not
promise immediate decode for every program in an MPTS.

- The export reuses the TS export's jitter buffer (`rs/moq-mux/src/jitter.rs`),
  keyed on each object's PCR time instead of a DTS, which also spreads each
  object's bytes at the PCR-implied rate. It must pace on the source's PCR,
  not on arrival: a pacer that re-clocks on arrival moves the PCR-to-PTS
  offset over a long capture and fails the decoder buffers, even though every
  byte is intact.
- An object that misses its deadline is dropped and counted, as the release
  stage does. The output shows a continuity error there; passthrough never
  rewrites continuity counters.
- Source clock drift is handled by the fixed-delay release's clock recovery,
  shared with the demultiplexed export, not here.
- A flagged forward jump arrives as a break marker before the group it
  starts; the release keeps the jump in the output, since the bytes carry it.
- Passthrough gives 1+1 identity for free: two exporters fed the same objects
  emit identical TS packets for every object both release, continuity
  counters included; an object one leg drops late is a gap in that leg only.
  That is TS identity, not ST 2022-7 recovery, which also needs matching RTP
  headers from [RTP egress](/quest/m2/ts-rtp-egress.md).
- Rust only.

This quest owns the reusable raw packet/position and release-schedule oracle
and its basic omission fixtures; the recovery quest extends it with the wider
fault matrix rather than building a second harness.

Automated tests in CI: an export of a supported capture is byte-identical to
the admitted input interval on a complete path, and the applicable T-STD check
gives the same verdict on output and input. Assert packet order and release
schedule as well as bytes. Two exporters fed the same objects with different arrival
skew emit identical bytes; when only one misses a deadline, its output is the
other's less that object's packets. A dropped object is counted, and the rest
still go out on their original schedule, leaving the missing time intact.
Use deterministic object/group omissions as well as a nightly real-network
impairment test: IP packet loss may be retransmitted and is not proof of
missing MoQ content. Fixtures include null/unknown PIDs, tables, adaptation
bytes, split PES, clock wrap, and MPTS with unaligned program recovery points.
Run TSDuck and FFmpeg automatically for profiles where their assertions apply;
record expected decode outages under loss separately from byte preservation.

Update `doc/bin/cli.md` for the flag, which today says `export ts` does not
read the passthrough track.

Public API: a new export mode; nothing existing breaks.

## Related

- [Deterministic TS legs](/quest/m2/ts-hitless.md) - the demultiplexed lane's common packet identity under `--sync`
- [T-STD TS export](/quest/m1/tstd/README.md) - the jitter buffer and clock recovery this release shares
- [TS validation](/test/ts/README.md) - shared automatic graders and controls
- [Loss and recovery](/quest/m1/ts-reliability/recovery.md) - actual missing content and reacquisition proof
- [Same-epoch importers](/quest/m2/hop-aligned-import.md) - identical common objects do not prove independent importers publish identical groups
