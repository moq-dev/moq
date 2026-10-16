# TS fidelity and recovery

## Goal

The two TS modes have explicit, software-validated contracts under clean
delivery, late join, and lost MoQ content. A dedicated guide explains which
mode to choose, supported profiles, acquisition, loss, and the automated
evidence behind each claim.

## Plan

Decided in the 2026-10-11 TS audit:

- TS to Hang to TS is the semantic mode. Remove packetization and stuffing;
  retain the supported source semantics through media tracks and catalog
  extensions. Hang's core stays container-neutral. Byte identity is not its
  contract.
- TS to MoQ to TS is the packet-preserving fallback. Keep an SPTS or an entire
  MPTS unchanged. Per-program tracks mean separately supplied SPTS inputs
  in separate raw broadcasts under the existing catalog contract,
  not extraction from an MPTS. Reject per-PID splitting and reversible packet
  scatter for this lane: both add reconstruction complexity to the fallback.
- Loss in the fallback emits unchanged surviving packets on their source
  schedule and reports gaps. Never silently repair continuity counters,
  insert tables or stuffing, or compress the missing interval. Exact bytes
  do not imply successful decoding or T-STD conformance after loss.
- Start with explicit software-validated profiles and expand with fixtures.
  Hardware receivers and commercial qualification are useful additional
  evidence, not prerequisites for repeatable CI. Do not claim universal
  broadcast compliance from the current subset of the buffer model.
- Every TS validation, including TSDuck and FFmpeg, is an automated test
  with assertions and a CI invocation. One-off analysis is diagnostic
  evidence, not completion. Long real-time tests may run nightly; short
  deterministic checks belong on relevant PRs.

The existing raw importer admits an aligned 188-byte suffix after acquisition;
it does not currently retain every byte supplied before that point. The guide
must distinguish full-input identity from identity over the admitted interval,
and distinguish a MoQ group boundary from a decoder acquisition point. RAI,
periodic cuts, and an intact object alone do not ensure available PAT/PMT,
codec configuration, clock context, or references for every MPTS program.

The README owns the guide and integration proof for the modes present in
m1: semantic remux and raw import/carriage. The deferred raw export quest
extends the guide and proves its output when it lands; it does not block
this proof. Reuse the children's fixtures and CI recipes, including their
known-bad controls. Publish
a coverage table linking each supported profile and loss guarantee to its
test; unsupported or unmeasured cells stay explicit. Keep CLI instructions
and the MPEG-TS draft consistent in each feature change.

Decided 2026-10-16 by the maintainer, for private PES timing and headers
(#5718):

- **Carriage.** A PES without a PTS is not untimed. The multiplex places
  every byte on the program clock; only its presentation time is absent.
  So a private PES track stays timed on the legacy container, which keeps
  the timestamp in its payload and so survives transports without
  net-level timestamps (IETF drafts 14-16, lite-05/06's send-time stamps),
  and never becomes an untimed track. Each frame's timestamp is the PES's
  source placement: the program clock at its first byte, interpolated
  between the PCRs on either side (ISO 13818-1 2.4.2.2) and moved onto the
  catalog clock like the media. Its payload is the whole PES packet: PTS
  and DTS rebased, every other header byte kept, so PTS presence, special
  stream-ID forms and optional fields round-trip without per-stream-type
  timing rules. A PES that carries a PTS also gets placement, not its PTS,
  as its frame timestamp; the PTS lives in the bytes.
- **Signalling.** A new `pesPacket` member of the `mpegts.tracks` entry,
  beside `verbatim` rather than a new `framing` value. Old consumers ignore
  an entry with neither `verbatim` nor a described track, so a published
  exporter silently drops these tracks (teletext, DVB subtitles, DVB AC-3,
  KLV, unknown private types) from the rebuilt TS and its PMT, with no
  error, instead of refusing the whole catalog. Chosen over the loud
  refusal so old exporters keep the media and sections.
- **Legacy `pes`.** The `pes` framing is removed, not kept for a release:
  export refuses a catalog that names it, and the AC-3 repack goes with
  it. So a current exporter refuses broadcasts from published importers
  and recordings that carry private PES, and draft -01 breaks -00 there.
  The deletion lands with the importer switch, so main never writes a
  catalog its exporter refuses.
- **Clock fields.** Only PTS and DTS are rebased. ESCR, `pack_header_field`,
  TREF and PES-level scrambling refuse their stream with a warning and an
  importer health count. TS-packet-layer details (continuity, adaptation
  fields, packet boundaries) stay with the raw fallback.

Rejected: an untimed track with a container that takes time from the net
frame, which loses timing on those transports and in archive replay and
needs timedness fixed before the first PES; a per-track "no PTS" flag,
which keeps the synthesized header and misleads old exporters; and
extrapolating placement from earlier PCRs without waiting, which is wrong
on variable-rate multiplexes.

The planning children settle catalog and clock contracts before production
changes. Their output adds independently completable implementation quests to
this line; an audit finding is not authorization to guess a public API.
PMT fragmentation (#5220), initial mux-rate measurement (#5287), and
decoder-time admission (#5306) have landed; PSI runs and catalog linger
remain with their current quests.

## Required

- [Plan: semantic fidelity](/quest/m1/ts-reliability/fidelity.md) - explicit profiles and PMT lifecycle contracts produce implementation quests
- [Raw stream types](/quest/m1/ts-reliability/stream-type.md) - unknown stream types survive export instead of failing the PMT
- [Whole-PES export](/quest/m1/ts-reliability/pes-export.md) - export writes `pesPacket` tracks byte for byte at their placement
- [Whole-PES import](/quest/m1/ts-reliability/pes-import.md) - private PES travels whole at its source placement, a missing PTS stays missing, and the `pes` framing is deleted
- [Section placement](/quest/m1/ts-reliability/section-placement.md) - sections share the PCR placement clock instead of the video PTS
- [Loss and recovery tests](/quest/m1/ts-reliability/recovery.md) - actual MoQ omissions exercise reacquisition and unaffected tracks
- [Data-track breaks](/quest/m1/ts-reliability/data-break.md) - a data track's declared break reads as a break, not as media lost upstream
- [Audio behind the keyframe](/quest/m1/ts-reliability/audio-lead.md) - an export whose video start group is dropped still begins on a keyframe
- [T-STD TS export](/quest/m1/tstd/README.md) - the semantic mux schedules the content it admits within its supported buffer models

## Related

- [Epoch anchor](/quest/m2/epoch-anchor.md) - shared identity across importers requires one source coordinate per epoch, derived from the epoch
- [TS over RTP legs](/quest/m2/ts-rtp-egress.md) - TS identity alone is not ST 2022-7, which also needs matching RTP headers
- [MSFTS mapping](/quest/m3/msfts-convergence.md) - document the exact equivalence and differences between the carriage modes

- [TS passthrough export](/quest/m2/ts-passthrough-export.md) - a new mode that does not gate correctness of existing TS modes
