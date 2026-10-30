# [M] Peer-reachable unsafe has invariant fuzz targets and runs under Miri

## Goal

The hand-written unsafe that peer input reaches in `moq-c`, `moq-uring`, and `moq-sock` runs under Miri or a fuzz target of its own, asserting more than "it did not panic". Other peer-facing decoders, such as the media importers and RTMP's AMF0, are outside this quest.

The moq-net half is done: the `ingest` and `request` targets cover the incremental group ingest and request-stream framing, and the seeds include prefixes at and past every cap. What remains: `rs/moq-c`, `rs/moq-uring`, and `rs/moq-sock` have no fuzz target and no Miri run. Their cmsg walks bound every length by the buffer, with unit tests, but nothing searches for the next mistake.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer:

- Add a target for `moq-sock`'s cmsg walk (`cmsg::Iter` and `ControlMetadata::decode` in `rs/moq-sock/src/udp/unix.rs`) and for `moq-uring`'s `RecvMeta::parse` (`rs/moq-uring/src/udp.rs`). Both walks must stop at a malformed length and keep what came before it: build well-formed control messages from the input, append a malformed header and arbitrary bytes, and assert the walk reads the same metadata as the well-formed prefix, and the metadata a reference model expects from it.
- Add a Miri job for the crates whose unsafe is reachable from a peer.
- Wire every target and the Miri run into the nightly gate, and keep the merge-gate replay of committed regressions.

The point is the invariant, not the coverage number: a target that only checks for a panic would miss the bugs this audit is about.

Each crate follows moq-net's shape (`rs/moq-net/fuzz/README.md`): the target body in a hidden `fuzz` module so the crate's own tests replay `fuzz/regressions/` on stable, a one-line shim per target, an `--exclude` in the `no_fuzz` list of `rs/justfile`, and a row in `nightly.yml`'s fuzz matrix. Target names must stay unique across crates.

Ordering:

- `moq-sock`'s `CMsg::decode` still reads `size_of::<T>()` past a short message (only a `debug_assert_eq!` checks it), which [Bound cmsg payload reads](/quest/m2/cmsg-decode-bound.md) fixes. A moq-sock target landing first would fail nightly on that known overread, so land it after, or with, that fix. The moq-uring target has no such dependency.
- [In-memory unsafe under Miri](/quest/m2/miri-memory.md) shares one `just rs miri` recipe and nightly job with this quest: whichever lands first creates it, and the other adds its crates.

Public API: none. Wire: none.

## Related

- [CI cost and latency](/quest/m0/ci/README.md) - where the nightly budget lives
- [Bound cmsg payload reads](/quest/m2/cmsg-decode-bound.md) - fixes the moq-sock overread a cmsg target would hit
- [In-memory unsafe under Miri](/quest/m2/miri-memory.md) - shares the Miri recipe and nightly job
- [Untrusted media parsers have fuzz targets](/quest/m2/media-fuzz.md) - the media parser targets beside these
