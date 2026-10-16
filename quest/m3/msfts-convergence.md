# [M] MPEG-TS lane converges with MSFTS ES-level carriage

## Goal

A subscriber author can map this repository's demultiplexed TS lane
(access units, Hang catalog `mpegts` section) onto MSFTS ES-level carriage
without guessing. Transporting TS verbatim is out of scope here:
`moq import ts --passthrough` carries it in an `m2ts` catalog section that
[the TS draft](/drafts/draft-lcurley-moq-mpegts.md) maps onto MSFTS's track
fields.

## Plan

Decided in the 2026-09-30 audit: moved from m4 and the msfts#33 gate removed.
msfts#33 closed on 2026-09-28, answered by msfts#36 (merged 2026-09-24), and
egress moved to msfts#37 (closed). Two differences remain:

- **Program tables.** PAT/PMT information lives in the Hang catalog's
  `mpegts` section; supported SI already uses tracks. Map these mechanisms
  separately to MSFTS's table tracks instead of saying all tables live in
  the catalog.
- **ES units.** MSFTS `es-packets` (draft-gregoire-moq-msfts-02) carry the
  TS packets of one PID. Here decoded media carries access units, and
  undecoded PES carries the whole PES packet at its source placement
  ([whole-PES import](/quest/m1/ts-reliability/pes-import.md)). Document the
  mapping between them.

Decided in the 2026-10-11 audit: keep the semantic lane and raw fallback
distinct. Whole SPTS maps to `unmodified-program`, whole MPTS to
`unmodified-multiplex`. MSFTS `per-program` extraction and `es-packets`
(which omit null packets) are not byte-identical whole-input carriage.
The fallback will not split PIDs or add source-position reconstruction
metadata. Use [the current draft](https://datatracker.ietf.org/doc/html/draft-gregoire-moq-msfts)
to document both semantic and scheduling differences; shared terminology
does not establish wire interoperability.

Update `drafts/draft-lcurley-moq-mpegts.md` and `doc/concept` with whatever
lands. [#4577](https://github.com/moq-dev/moq/pull/4577) (per-ES access
units at export) has merged, and [#4579](https://github.com/moq-dev/moq/pull/4579)
(export on the mux rate) closed in favour of the
[T-STD line](/quest/m1/tstd/README.md).
[#4580](https://github.com/moq-dev/moq/pull/4580) (per-program SI) merged on
2026-10-01 and touches the same area.
