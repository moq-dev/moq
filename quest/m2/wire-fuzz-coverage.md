# [L] The audited moq-net decoders and peer-reachable unsafe have invariant fuzz targets

## Goal

The moq-net decoders the audit found uncovered are fuzzed with an assertion stronger than "it did not panic", and the hand-written unsafe that peer input reaches in `moq-c`, `moq-uring`, and `moq-sock` runs under Miri or a target of its own. Other peer-facing decoders, such as the media importers and RTMP's AMF0, are outside this quest.

`ietf_wire` already covers both SETUP envelopes and the Setup Options a session reads from them. The gaps that remain:

- The incremental frame and group ingest (`lite::subscriber::FrameIngest`, `ietf::subscriber::GroupIngest`/`SubgroupRecv`, the `lite::ietf` state/object/property decoders) has no target.
- Request-stream framing (`Body`, `Framed`, `FollowUp`, `Next`, `whole_message`) is only unit-tested.
- Every seed body is six bytes or less, so no length-prefix or count rejection path is exercised near its cap.
- `rs/moq-c`, `rs/moq-uring`, and `rs/moq-sock` have no fuzz target and no Miri run. Their cmsg walks now bound every length by the buffer, with unit tests, but nothing searches for the next mistake.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer:

- Add one target for a stream of frame headers, records, and payloads through the incremental ingest, asserting the declared sizes and the stream position stay consistent.
- Add a target for request-stream framing.
- Add seeds that approach a limit (`0xffff` and 64 MiB prefixes) so the rejection paths run.
- Add the crates whose unsafe is reachable from a peer to `just rs miri` (`sh/rs/miri.sh`), which Nightly already runs for `kio` and `moq-net`, and a target for `moq-sock`'s cmsg walk and `moq-uring`'s `RecvMeta::parse`. Both walks must stop at a malformed length and keep what came before it.
- Wire every target and the Miri run into the nightly gate, and keep the merge-gate replay of committed regressions.

The point is the invariant, not the coverage number: a target that only checks for a panic would miss the bugs this audit is about.

Public API: none. Wire: none.

## Related

- [CI cost and latency](/quest/m0/ci/README.md) - where the nightly budget lives
