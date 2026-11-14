# [L] Gateway and format parsers have fuzz targets

## Goal

Each parser that reads bytes from a peer or a file outside the relay has a
cargo-fuzz target asserting an invariant, in Nightly's fuzz matrix with its
regressions replayed in `just check`: moq-webrtc's STUN, SDP, and RTP
depacketizers, moq-rtsp, moq-srt, moq-hls playlists, moq-json, moq-loc,
moq-msf, and the hang catalog. A parser left out is listed with the reason.

## Plan

Decided 2026-11-13 (maintainer, /quest-plan on what testing to add): these have unit tests and no fuzzing (moq-webrtc has about 1,680 tests
and no target).

- ✅ m4 (rejected: m1 with the relay-reachable targets): a gateway or an
  importer runs beside the relay, so a crash there costs one ingest.

Agent's choices, for the implementer to revisit:

- Order by exposure: the listeners an unauthenticated peer reaches (WebRTC,
  RTSP, SRT) before the formats read from a trusted publisher or a file.
- moq-webrtc's parsers are vendored (`rs/moq-webrtc/src/vendor`): fuzz them
  where they sit, and report a finding upstream as well as fixing it here.
- The same shape as [the relay parsers](/quest/m1/fuzz-relay-parsers.md).
  Where JS has the same decoder (JSON, the catalog, MSF), commit findings to
  the shared `fixtures/` corpus.
- If the first runs find more than one PR can fix, land the targets with
  their regressions and plan a quest per finding.

Public API: none. Wire: none.

## Related

- [OSS-Fuzz](/quest/m4/oss-fuzz.md) - runs these around the clock once it lands
