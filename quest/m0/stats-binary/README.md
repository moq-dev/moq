# [XS] Stats wire contract

## Goal

Every stats track is documented as a wire contract a consumer in any
language can read: the path layout, tiers, the three track kinds, both JSON
encodings (`.json` and `.json.z`), and the counter semantics. The JSON tracks
stay unchanged on the wire.

## Plan

The allocation-free drain landed in #3918 and #3920. The FlatBuffers `.fb.z`
flavor this line planned was abandoned: a prototype lost to `.json.z` on
bytes at every size, and by up to 15x at 4096 broadcasts, because a full
snapshot outgrows DEFLATE's 32 KiB window while `.json.z` sends merge-patch
deltas. It did win on decode CPU (about 10x) and on allocations on both
sides; the numbers and the prototype are in the PR that abandoned it.

A typed binary contract still needs deltas to compete on bytes, or a
compressor with a larger window than a browser's `deflate-raw`.
[Binary delta stats](/quest/m2/stats-delta.md) takes the delta route, outside
this line.

The contract lives at [doc/concept/stats.md](/doc/concept/stats.md); what
remains is landing the line on `main`.

## Related

- [Client stats](/quest/m1/qos/stats/README.md) - the per-broadcast extension the page will describe once it lands
