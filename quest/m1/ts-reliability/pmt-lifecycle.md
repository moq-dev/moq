# [L] Live PAT and PMT changes keep track names honest

## Goal

A live TS source that revises its PMT or PAT mid-stream produces a broadcast
whose every track name still means one elementary stream, and a TS export
that announces the revised layout. Added, removed and retyped streams, changed
descriptors, PID reuse and program changes each have one observable outcome:
carried under a fresh name, retired, or refused with an error. None is
silently kept stale.

## Plan

Reproduced 2026-10-20 in the fidelity audit. A PMT that retypes the private PES
on a PID from 0x06 (`KLVA` registration) to 0x15 (metadata descriptor) keeps
the `pesPacket` track, its catalog entry still says 0x06 with the old
descriptor, and the 0x15 payloads publish under the same name. A later PMT
that drops the PID leaves the entry and track in the catalog.

What the code does today:

- `Import::handle_pmt` only walks the streams the new PMT lists. Nothing
  retires a PID it no longer lists, and `ensure_stream` returns early for any
  PID that already has a route, so a new `stream_type` or ES descriptor loop
  never reaches it. Only a section/PES flip and the PCR transitions re-route.
- Catalog fields are captured once: `program_descriptors`
  (`program_recorded`), program identity (`identity_recorded`), and a decoded
  track's ES descriptors (`recorded_media`). The decoded codec importers fix
  their config from the first header; the identity guard for that lands in
  [Rust identity enforcement](/quest/m1/catalog-identity-rs.md), and this
  quest owns the PMT-level remap.
- A PAT naming another PMT PID adds a reassembler but never drops the old
  one. `Programs` ignores a program a later PAT adds, without a warning.
- `Export` builds the PAT/PMT once. A track added afterwards fails the export
  ("TS track layout changed after PAT/PMT was emitted"), and a track that
  leaves the catalog keeps its PID in the PMT forever. A switch to another
  instance already rebuilds the tables at the next `version_number`.

Decided 2026-10-20 by the maintainer: change in place with fresh names.

- A changed `stream_type` or ES descriptor loop finishes the old track,
  drops its catalog entry, and opens a new uniquely named track, as for a
  new PID. A removed stream finishes its track and leaves the catalog.
  `programDescriptors` updates in place, and a CUEI change re-routes the
  affected PIDs.
- Export rebuilds the PMT at the next `version_number` when the track set
  or an entry changes, drops a retired track's PID once its track ends, and
  keeps the PCR PID unless its track retires. A TS receiver sees an
  ordinary PMT revision and a Hang player sees a rendition switch; nothing
  else blips.
- A change of program identity (`transport_stream_id`, `program_number`)
  refuses with an error: the carried SI describes the old service.
- A new PMT PID for the same program moves the reassembler and updates
  `program.pmtPid`, which the export follows with a new PAT. `Programs`
  publishes a program a later PAT adds as its own broadcast, as it does for
  the first PAT, rather than ignoring it.

Why: it matches the 2026-10-15 identity decision (changed content gets a new
name; a new epoch stays reserved for a whole-broadcast restart). Rejected:
replacing the broadcast with a new epoch on any ES layout change, which makes
every subscriber reacquire every track so an audio-language add interrupts
video too; and refusing every layout change, which stops the ingest on common
live transitions such as a regional opt-out adding audio or a subtitle PID
coming and going.

Rust: `rs/moq-mux` TS import and export. JS: none; `js/msf` passes the
`mpegts` section through untyped and nothing in `js/` muxes TS. Draft
([mpegts](/drafts/draft-lcurley-moq-mpegts.md)): a publisher MUST NOT change
an entry's `pid`, `descriptors`, or carriage record in place, and the rebuild
section says how a consumer revises the PMT. Hang's core stays TS-free.

Tests use the generated `fixture::Mux` builder: PMT add, remove, retype,
descriptor-only change, version rollover (31 to 0), PID reuse after removal,
program descriptor and CUEI changes, PMT PID move, and an identity change
that must refuse. Each asserts the catalog, the track names, and the exported
PMT. Extend the TSDuck round trip with a generated mid-stream PMT revision so
`compliance.py` checks the rebuilt version sequence in CI.

## Related

- [Rust identity enforcement](/quest/m1/catalog-identity-rs.md) - codec config changes behind one PID mint names the same way
- [Stream routes](/quest/m1/ts-reliability/stream-routes.md) - which route a revised stream type gets
