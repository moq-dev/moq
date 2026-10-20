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

Private PES (decided 2026-10-16, #5718) landed in #5832 and #5921: a
private PES rides whole as a timed `pesPacket` track, stamped with its
program-clock placement, and the draft -00 `pes` framing is refused. The
guide documents that contract and the clock fields that refuse a stream.

The planning children settle catalog and clock contracts before production
changes. Their output adds independently completable implementation quests to
this line; an audit finding is not authorization to guess a public API.
PMT fragmentation (#5220), initial mux-rate measurement (#5287), and
decoder-time admission (#5306) have landed; PSI runs and catalog linger
remain with their current quests.

## Required

- [Plan: semantic fidelity](/quest/m1/ts-reliability/fidelity.md) - explicit profiles and PMT lifecycle contracts produce implementation quests
- [Loss and recovery tests](/quest/m1/ts-reliability/recovery.md) - actual MoQ omissions exercise reacquisition and unaffected tracks

## Related

- [TS over RTP legs](/quest/m2/ts-rtp-egress.md) - TS identity alone is not ST 2022-7, which also needs matching RTP headers
- [TS health counters](/quest/m2/ts-health-stats.md) - ingress and egress evidence need distinct monitoring points
- [MSFTS mapping](/quest/m3/msfts-convergence.md) - document the exact equivalence and differences between the carriage modes

