# [S] The audited moq-net decoders and peer-reachable unsafe have invariant fuzz targets

## Goal

The uncovered moq-net decoders have invariant fuzz coverage, and the hand-written unsafe reached by input in moq-c, moq-uring, and moq-sock runs under Miri or a target of its own. Media importers and RTMP AMF0 are outside this quest.

## Plan

Current checkpoint:

- The IETF wire target now dispatches unified draft-17+ SETUP and legacy client/server SETUP through their actual envelope decoders, then parses the contained parameter block. Generated valid SETUP seeds reach the new arms. This uses only the existing test/fuzz gate, with no production feature or public API change.
- A test-only malformed-control invariant reproduces an arithmetic panic in moq-uring's RecvMeta::parse: a valid GRO cmsg followed by a header whose cmsg_len is usize::MAX overflows offset + message_len. The function promises to stop at a malformed suffix and preserve the valid prefix. Production parsing is unchanged.
- These lengths originate in kernel ancillary data, so this reproduction does not establish that a remote peer can choose them or exploit the panic.

The runtime repair is outside the authorized M2 tooling scope. Keep this draft blocked until a runtime fix is authorized separately. The minimal candidate is a checked end offset, or a comparison against the remaining slice length, before adding the length. Preserve the invariant and its input rather than weakening the test.

Remaining coverage work:

- Restore the existing draft setup regression inputs through the new SETUP envelopes; they still dispatch through Parameters.
- Exercise typed SETUP option handling as well as the generic Parameters decoder.
- Fuzz incremental frame/group ingest and the lite IETF state/object/property decoders, asserting declared payload sizes and consumed stream positions.
- Fuzz request framing (Body, Framed, FollowUp, Next, and whole_message). No request target is included in this checkpoint.
- Add near-cap length/count seeds, including 0xffff and 64 MiB prefixes.
- Add moq-sock cmsg and moq-uring RecvMeta targets, Miri for moq-c/moq-uring/moq-sock, and nightly wiring. Keep stable replay of committed regression inputs.

The point is the invariant, not the coverage number. All additions must remain test/fuzz/CI tooling; supported production behavior, public API, wire, and versions stay unchanged.

## Related

- [CI cost and latency](/quest/m0/ci/README.md) - where the nightly budget lives
