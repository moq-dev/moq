# [M] HLS export lists a same-codec reconfigure's first group under the new init

## Goal

When a rendition changes resolution (or any other decoder-affecting field) while keeping its codec string, the first group after the change is listed under the new init and segment tag, whichever of the catalog and the HLS records reaches the export first.

The conformance test in [moq-dev/moq#7387](https://github.com/moq-dev/moq/pull/7387) is the end-to-end regression test: its `video1` same-codec switch must pass.

## Plan

Found by the conformance test in [moq-dev/moq#7387](https://github.com/moq-dev/moq/pull/7387). The export already replaces the instance on a decoder-identity change (the same fields hang's `same_identity` compares, plus geometry), and since [moq-dev/moq#7436](https://github.com/moq-dev/moq/pull/7436) the playlist keeps the retired instance's segments, under their own init, ahead of the replacement's behind a discontinuity (RFC 8216 6.2.2).

What remains is the hand-over race. The catalog and the records are separate tracks, so the new config's first group can reach the old instance's records watcher before the catalog update. The retiring instance claims it, so it is muxed under the old init and the replacement starts one group late.

Waiting on a plan: the maintainer decided on 2026-11-02 to remove the catalog's `hls` field (`rs/hang/src/catalog/hls.rs` and the JS zod schema), planned separately. That changes how the export finds a rendition's records, so the hand-over pick is designed after that plan, not with a catalog `start` field, a record track per config, or a marker in each record (all three dropped the same day). Once the removal is planned, add it under `Required` here or fold the pick into it.

Public API: none expected. Wire: decided by the removal plan.

## Related

- [HLS and DASH segment conformance](/quest/m1/hls-conformance.md) - the test that found it, which waits on this
