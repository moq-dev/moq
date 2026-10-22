# [S] The audited moq-net decoders and peer-reachable unsafe have invariant fuzz targets

## Goal

The moq-net decoders the audit found uncovered are fuzzed with an assertion stronger than "it did not panic", and the hand-written unsafe that peer input reaches in `moq-c`, `moq-uring`, and `moq-sock` runs under Miri or a target of its own. Other peer-facing decoders, such as the media importers and RTMP's AMF0, are outside this quest.

Today `rs/moq-net/fuzz` covers lite, IETF messages, announces, varint, path, and pattern. The gaps the audit found:

- The IETF SETUP decoder has no arm in `ietf_wire`. The old `draft*-setup-*` regression inputs now replay through the `Parameters` kind, so the u16 size, delta-encoded KVP block, and `decode_params!` path is uncovered.
- The incremental frame and group ingest (`lite::subscriber::FrameIngest`, `ietf::subscriber::GroupIngest`/`SubgroupRecv`, the `lite::ietf` state/object/property decoders) has no target.
- Request-stream framing (`Body`, `Framed`, `FollowUp`, `Next`, `whole_message`) is only unit-tested.
- Every seed body is six bytes or less, so no length-prefix or count rejection path is exercised near its cap.
- `rs/moq-c`, `rs/moq-uring`, and `rs/moq-sock` have no fuzz target and no Miri run, though `cmsg` walking and the io_uring address parsing are exactly where a bounded-buffer mistake would live.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer:

- Add the SETUP kinds to `ietf_wire` and restore the regression inputs through them.
- Add one target for a stream of frame headers, records, and payloads through the incremental ingest, asserting the declared sizes and the stream position stay consistent.
- Add seeds that approach a limit (`0xffff` and 64 MiB prefixes) so the rejection paths run.
- Add a Miri job for the crates whose unsafe is reachable from a peer, and a target for `moq-sock`'s cmsg walk and `moq-uring`'s `RecvMeta::parse`.
- Wire every target and the Miri run into the nightly gate, and keep the merge-gate replay of committed regressions.

The point is the invariant, not the coverage number: a target that only checks for a panic would miss the bugs this audit is about.

Public API: none. Wire: none.

## Related

- [moq-net internals](/quest/m1/net-internals.md) - the session drivers these targets exercise
- [CI cost and latency](/quest/m0/ci/README.md) - where the nightly budget lives
