# [M] Benchmark a Value merge-patch diff for moq-json

## Goal

Numbers decide whether `moq-json`'s generic serializer diff stays.
`rs/moq-json/src/diff.rs` is about 1,100 code lines (`Differ`, `lockstep`,
`MapDiff`, `Memo`) that diff a `Serialize` value against the last frame.
The candidate replacement is one non-generic `serde_json::Value`-vs-`Value`
RFC 7396 merge-patch diff, ported from `js/json/src/diff.ts` (about 120
lines). It keeps the same semantics, where a `null` in the new value forces a
snapshot, and passes the shared `rs/moq-json/tests/vectors.json`.

The quest ends with the chosen variant landed, or with the current diff kept
and the measurements recorded here or in the PR.

## Plan

Compare three variants:

1. The current diff (main).
2. The `Value` diff: serialize `T` to a `Value` once, then diff it against the
   previous `Value`.
3. Compression only: deltas off, a full snapshot per frame in the same group,
   and group DEFLATE through the track's `with_compress`.

Measure each on producer CPU, consumer CPU, wire bytes after DEFLATE,
allocations, and code size (`cargo llvm-lines` on a consumer crate such as
`moq-hls`, plus the `moq-json` line count). Use
`cargo bench -p moq-json --bench codec`, `--bench table` (100 to 20,000 rows,
1%, 10% and 100% changed) and `--bench allocations`, plus
`cargo bench -p moq-stats --bench aggregate`. Run on a quiet host, with a
main-vs-main run first to establish the noise floor.

Context:

- The maintainer asked, reviewing
  [moq-dev/moq#7400](https://github.com/moq-dev/moq/pull/7400): "why is it so
  complicated, are we chasing a dragon?" #7400 already moved the diff to
  serialized bytes so it compiles once, at +2–5% on table deltas up to 1,000
  rows and +6–12% at 10,000–20,000 rows.
- Maintainer: "In the past we tried relying on compression only (moq-flate)
  and the ratio was about the same, but the performance was different."
- History: [moq-dev/moq#1912](https://github.com/moq-dev/moq/pull/1912)
  compared merge plus DEFLATE against DEFLATE only.
  [moq-dev/moq#3955](https://github.com/moq-dev/moq/pull/3955) found full
  snapshots plus DEFLATE lose up to 15x in bytes once frames exceed DEFLATE's
  32 KiB window, and browsers lack zstd.
- Readers keep `delta()` regardless of the outcome. Deployed producers emit
  patches: `moq-stats`, `moq-boy`, `moq-cli report`, JS room metadata, and the
  bindings, which default deltas on.
- `drafts/draft-lcurley-moq-hang.md` contradicts itself on deltas: line ~149
  forbids multiple frames per group, while lines ~611 and ~803 allow RFC 7396
  patches after the snapshot. Align it with whatever this decides.
- `moq_json::diff<T>` has no in-repo callers since #7400. With a `Value` diff
  it could take `&Value`, or be removed.

## Related

- [Compile time audit](/quest/m2/audit-compile-time.md)
- [Artifact size audit](/quest/m2/audit-size.md)
