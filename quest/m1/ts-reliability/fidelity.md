# [M] Plan: explicit semantic TS profiles and live table changes

## Goal

Turn the semantic-lane audit into explicit supported profiles and focused
implementation quests. Each accepted mechanism is preserved, intentionally
normalized with a documented meaning, or refused. Nothing advertised as
supported disappears silently.

## Plan

Decided 2026-10-11: software-validated profiles first, then incremental
coverage. Broad MPEG-2/ATSC/private-data support is not a prerequisite for
shipping an honest narrower profile. The raw fallback stays an explicit
mode, not an automatic mid-stream switch of the catalog contract.

Audit findings to reproduce and scope:

- `Import::handle_pmt` adds streams but does not retire removed PES PIDs;
  an existing stream bypasses updated types/descriptors. Several catalog
  fields are captured only when a track is created. Export rejects a newly
  added ES once tables exist, while removals leave the initial PSI layout.
  Settle PMT version transitions, PID reuse, program/PCR changes, and track
  identity together. A name must never identify different content after a
  restart or PID reuse.
- MPEG-1/2 video is handled as a clock stream and its payload discarded.
  Unknown stream-type bytes can import as opaque but fail export's closed
  enum conversion. A known stream type is not proof of supported carriage.
- SI ingest recognizes fixed DVB PIDs. ATSC PSIP, CAT/conditional access,
  dynamically signalled tables, descriptors and private sections need
  explicit profile decisions. Packet passthrough can retain mechanisms the
  semantic lane does not understand.
- Section export requires video, leaving audio-only programs with sections
  unsupported. Keep opaque SCTE-35 fidelity separate from the later typed
  browser cue API; basic carriage should not depend on that API existing.
- Private PES loses optional header semantics today; the
  [whole-PES quests](/quest/m1/ts-reliability/pes-import.md) carry it
  byte-exact, and [raw stream types](/quest/m1/ts-reliability/stream-type.md)
  fixes the export enum conversion.

The maintainer decides whether a supported profile handles live PMT changes
in place with fresh track identities, replaces the broadcast, or initially
refuses specific transitions. Present the observable tradeoffs and ask before
choosing the public shape. Build a mechanism-to-catalog/track/refusal table
and use it to produce independently completable quests, with Rust/JS/draft
impact recorded for every extension. Keep Hang's core TS-free.

Every implementation quest includes generated or redistributable fixtures
and an automated round-trip or refusal test in CI. Include PMT add/remove,
version rollover and PID reuse, unknown stream types, audio-only plus
sections, missing timing, and the profile's descriptors. Preserve raw
section/PES bytes only where the settled contract actually promises that.

## Related

- [Whole-PES import](/quest/m1/ts-reliability/pes-import.md) - private PES keeps its header bytes and source placement
- [Automated validation](/test/ts/README.md) - profile declarations link to executable evidence
- [Typed SCTE-35](/quest/m3/2279-hang-typed-scte-35-ad-cue-signaling-carried-opaquely.md) - application cue APIs follow basic opaque carriage
