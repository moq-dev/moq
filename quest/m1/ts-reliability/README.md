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
m1: semantic remux and raw import/carriage. Raw export and its packet-level
proof landed in #5856; the guide covers it too. Reuse the children's
fixtures and CI recipes, including their known-bad controls. Publish
a coverage table linking each supported profile and loss guarantee to its
test; unsupported or unmeasured cells stay explicit. Keep CLI instructions
and the MPEG-TS draft consistent in each feature change.

Private PES (decided 2026-10-16, #5718) landed in #5832 and #5921: a
private PES rides whole as a timed `pesPacket` track, stamped with its
program-clock placement, and the draft -00 `pes` framing is refused. The
guide documents that contract and the clock fields that refuse a stream.

The semantic fidelity audit (2026-10-20) maps each source mechanism to its
contract in the semantic lane. The guide's profile table starts from it;
each open row has a child quest, and an audit finding is not authorization
to guess a public API. Preserved means carried through the catalog or a
track and re-emitted; normalized means rebuilt with the stated meaning;
refused means an error or a counted drop.

| Mechanism | Contract | Owner |
|:--|:--|:--|
| H.264 (0x1B), H.265 (0x24) | Preserved: video rendition, PID and ES descriptors in `mpegts.tracks`; AUs cross PES boundaries with source or explicitly proportional POC-derived PTS; DTS normalized from decode order | landed |
| AAC ADTS (0x0F), MP2, AC-3, E-AC-3, Opus (0x06 + `Opus`) | Preserved: audio rendition; Opus export labels mono and stereo, and 3 to 8 channels with an OpusHead in the Vorbis layout. Other mapping families (such as 255) refuse | landed |
| Private and unknown PES types, LATM (0x11), metadata (0x15) | Preserved: `pesPacket`, PTS and DTS rebased | landed |
| PES clock fields (ESCR, pack header, TREF), PES scrambling | Refused, counted in `refused` | landed |
| Private sections (0x05), SCTE-35 (0x86 + `CUEI`) | Preserved: `verbatim` section, `pts_adjustment` rebased | landed |
| MPEG-1/2 video (0x01/0x02) | Preserved: `pesPacket` grouped from one sequence header to the next, keeping its clock role | landed |
| DSM-CC types (0x0A to 0x0D), 0x86 without `CUEI` | DSM-CC preserved as a `verbatim` section; 0x86 without `CUEI` refused, counted in `refused` | landed |
| CAT, EMM/ECM, ATSC PSIP, other SI PIDs, unlisted PIDs, TS scrambling | Outside the profile, raw lane; counted in `refused`, unclaimed PIDs when the import reads every program | landed |
| DVB SI on 0x0010, 0x0011, 0x0012, 0x0014 | Preserved: snapshot track per PID and `table_id` | landed |
| Program identity, program and ES descriptors | Preserved: descriptors follow each PMT revision; an identity change refuses | landed |
| PMT add, remove, retype; PID reuse | Normalized without a new epoch: a changed or reused PID ends its track and continues under a fresh name, and the export rebuilds the PMT at the next `version_number` | landed |
| PAT changes: moved PMT PID, programs added or removed | Normalized: followed; a removed program retires, and returns as a new epoch, and a running export moves the PMT behind a PAT at the next `version_number` | landed |
| Programs without video | Preserved when the PMT names a PCR PID; refused without one | landed |
| PCR | Normalized: rebased, carried on the first video PID, else audio, else a dedicated PCR-only PID | landed |
| Mux rate, null stuffing | Normalized: re-padded to `muxRate` while the source is CBR | landed |
| PSI and SI repetition | Normalized: PAT/PMT every 500 ms and at keyframes, SI at DVB maxima | landed |
| Media PES headers, TS packet layer | Normalized: rebuilt by export; the raw lane keeps them | landed |
| PAT network PID entry | Preserved: listed on 0x0010 when a NIT is carried | landed |

Decided in the 2026-10-22 root-cause audit: the rows above kept getting
patched one at a time because two foundations were missing. Decode time is
reconstructed from history instead of decode order, and import state has no
owner that retires it. Those quests rank ahead of the remaining rows. Rejected: carrying
decode time in every frame container, one shared ingest byte budget,
and replacing the demux with GStreamer or another parser (it fixes neither
cause). Out of scope for now: decoupling catalog publication from mux-rate
measurement, except for a supervised restart, which reuses its predecessor's
measurement (decided 2026-10-28). Re-planned the same day: decode order covers MKV and FLV too and
absorbs the release audit's export-reorder quest, and the shared parser lands
before the open stream-routes and PAT drafts, which rebase onto it.

PMT fragmentation (#5220), initial mux-rate measurement (#5287), and
decoder-time admission (#5306) have landed, as have PSI runs (#5606) and
catalog linger (#5305).

## Required

- [A small step back keeps the TS export's video](/quest/m1/ts-reliability/step-back.md) - the export re-acquires on a sub-group step back from any source instead of dropping its video
- [Shared PID stats](/quest/m1/ts-reliability/shared-pid-stats.md) - `ts::stats` counts a shared PID once in the shared parser, so its timing counts survive program changes

## Related

- [MSFTS mapping](/quest/m3/msfts-convergence.md) - document the exact equivalence and differences between the carriage modes

