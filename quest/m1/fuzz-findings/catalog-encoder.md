# [M] A catalog update reuses its encoder

## Goal

Republishing a compressed track's latest value, as every hang catalog and
HLS update does, no longer allocates a fresh DEFLATE encoder (about 200 KB) per
group. In the `annexb` target, the 202-byte input that caused 24 encoders
and about 4.9 MB of allocation stays inside the fuzz bound, and is committed
under `rs/moq-mux/fuzz/regressions/`.

## Plan

Decided 2026-10-30 (maintainer): the encoder per update is a bug, not an
accepted cost, so the fuzz bound gets no per-update allowance.

Each compressed group is its own DEFLATE stream (draft-lcurley-moq-flate),
so `Flate::deflate` in `rs/moq-net/src/model/group.rs` creates a
`flate::Encoder` on a group's first write and drops it at `finish`. A
publisher that writes one frame per group, as a catalog does, pays the
allocation on every update.

Recommended: keep a finished group's encoder, reset with `Compress::reset`,
on the track's cache account (`cache::Track`) and hand it to the next group
the same track opens, billed to the pool while parked so the cache budget
still counts it. At most one spare per track, so concurrent groups still
allocate their own. The spare lives as long as the track; say in the PR
whether idle compressed tracks holding it is acceptable. The wire format
does not change: a reset encoder starts a cold window, as a new one does.

Public API: none. Wire: none.
