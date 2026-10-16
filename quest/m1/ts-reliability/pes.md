# [M] TS import assembles PES headers across packets with bounded staging

## Goal

Legal clear PES headers can span TS packets, and malformed or incomplete
input cannot grow importer staging indefinitely. Damage cannot concatenate
bytes across a lost packet into a plausible access unit.

## Plan

The audit found `Import::handle_pes` parses the first packet's payload as a
complete PES header. `PesStart::parse` needs the complete optional header,
so a legal clear header split across TS packets is discarded. PES payload
continuations and sections waiting for timing also have no explicit byte
bound in their staging paths. Raw passthrough already has explicit bounds;
semantic demux needs equally clear limits appropriate to its own units.

Reassemble the header before parsing or delivering its payload. Keep the
state machine small and distinguish incomplete input from malformed input.
Honor declared lengths, continuity damage, discontinuity, duplicate packets,
new PUSI, and end of input. Special PES stream IDs have different header
rules and must not be forced through the ordinary optional-header layout.

Bound retained bytes while waiting for a complete header, an unbounded PES's
next boundary, or missing clock/timestamp information. Derive protocol limits
where they exist, and use the repository's resource-limit conventions for
the rest. Refuse unsupported input explicitly instead of allowing unbounded
growth or silently inventing timing. A public configuration change needs its
own API decision if existing limits cannot express the policy.

Regression tests fragment legal headers at every packet boundary and prove
payload/timestamps survive. Negative cases cover header length overruns,
truncation, continuity loss, and streams that never supply the awaited
boundary or clock. Assert retained memory is bounded and no damaged unit
escapes. Run these in the Rust CI tests; add a generated fragmented-header
round trip to the automated TSDuck/FFmpeg harness.

The nonzero PES length underflow is already fixed separately in #5323;
retain its regression rather than duplicating that fix.

## Related

- [Automated validation](/test/ts/README.md) - independent parsing and decoding of the reassembled output
- [PES timing and headers](/quest/m1/plan-ts-pes-untimed.md) - defines semantics retained after assembly
