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

The semantic fidelity audit (2026-10-20) maps each source mechanism to its
contract in the semantic lane. The guide's profile table starts from it;
each open row has a child quest, and an audit finding is not authorization
to guess a public API. Preserved means carried through the catalog or a
track and re-emitted; normalized means rebuilt with the stated meaning;
refused means an error or a counted drop.

| Mechanism | Contract | Owner |
|:--|:--|:--|
| H.264 (0x1B), H.265 (0x24) | Preserved: video rendition, PID and ES descriptors in `mpegts.tracks` | landed |
| AAC ADTS (0x0F), MP2, AC-3, E-AC-3, Opus (0x06 + `Opus`) | Preserved: audio rendition; Opus export labels mono and stereo only | landed |
| Private and unknown PES types, LATM (0x11), metadata (0x15) | Preserved: `pesPacket`, PTS and DTS rebased | landed |
| PES clock fields (ESCR, pack header, TREF), PES scrambling | Refused, counted in `refused` | landed |
| Private sections (0x05), SCTE-35 (0x86 + `CUEI`) | Preserved: `verbatim` section, `pts_adjustment` rebased | landed |
| MPEG-1/2 video (0x01/0x02) | Preserved: `pesPacket`, keeping its clock role; today clock only, payload discarded | [routes](/quest/m1/ts-reliability/stream-routes.md) |
| DSM-CC types (0x0A to 0x0D), 0x86 without `CUEI` | Today PES-parsed into damage, or dropped uncounted | [routes](/quest/m1/ts-reliability/stream-routes.md) |
| CAT, EMM/ECM, ATSC PSIP, other SI PIDs, unlisted PIDs, TS scrambling | Outside the profile, raw lane; today dropped uncounted | [routes](/quest/m1/ts-reliability/stream-routes.md) |
| DVB SI on 0x0010, 0x0011, 0x0012, 0x0014 | Preserved: snapshot track per PID and `table_id` | landed |
| Program identity, program and ES descriptors | Preserved, but captured once | [PMT changes](/quest/m1/ts-reliability/pmt-lifecycle.md) |
| PMT add, remove, retype; PID reuse; PAT changes | Normalized: in place under fresh track names, PMT rebuilt at the next `version_number`; program identity changes refused. Today stale tracks on import, refusal or stale PMT on export | [PMT changes](/quest/m1/ts-reliability/pmt-lifecycle.md) |
| Programs without video | Today audio plus `pesPacket` only; sections or no media refused | [clockless export](/quest/m1/ts-reliability/clockless-export.md) |
| PCR | Normalized: rebased, carried on the first video (else audio) PID | landed |
| Mux rate, null stuffing | Normalized: re-padded to `muxRate` while the source is CBR | landed |
| PSI and SI repetition | Normalized: PAT/PMT every 500 ms and at keyframes, SI at DVB maxima | landed |
| Media PES headers, TS packet layer, PAT network PID entry | Normalized: rebuilt by export; the raw lane keeps them | landed |

PMT fragmentation (#5220), initial mux-rate measurement (#5287), and
decoder-time admission (#5306) have landed; PSI runs and catalog linger
remain with their current quests.

## Required

- [Live PAT/PMT changes](/quest/m1/ts-reliability/pmt-lifecycle.md) - a revised PMT or PAT never leaves a track name meaning other content
- [Loss and recovery tests](/quest/m1/ts-reliability/recovery.md) - actual MoQ omissions exercise reacquisition and unaffected tracks
- [Stream routes](/quest/m1/ts-reliability/stream-routes.md) - every PID gets a declared route, and what is not carried is counted
- [Clockless export](/quest/m1/ts-reliability/clockless-export.md) - TS export rebuilds audio-only and data-only programs

## Related

- [TS over RTP legs](/quest/m2/ts-rtp-egress.md) - TS identity alone is not ST 2022-7, which also needs matching RTP headers
- [TS health counters](/quest/m2/ts-health-stats.md) - ingress and egress evidence need distinct monitoring points
- [MSFTS mapping](/quest/m3/msfts-convergence.md) - document the exact equivalence and differences between the carriage modes

