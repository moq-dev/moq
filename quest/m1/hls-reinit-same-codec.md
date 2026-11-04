# [M] HLS export lists a same-codec reconfigure's first group under the new init

## Goal

When a rendition changes resolution (or any other decoder-affecting field) while keeping its codec string, the first group after the change is listed under the new init and segment tag, whichever of the catalog and the HLS records reaches the export first.

A regression test delivers the new config's first group to the export ahead of the catalog update and finds it under the new init.

## Plan

Found by the conformance test in [moq-dev/moq#7387](https://github.com/moq-dev/moq/pull/7387). The export already replaces the instance on a decoder-identity change (the same fields hang's `same_identity` compares, plus geometry), and since [moq-dev/moq#7436](https://github.com/moq-dev/moq/pull/7436) the playlist keeps the retired instance's segments, under their own init, ahead of the replacement's behind a discontinuity (RFC 8216 6.2.2).

What remains is the hand-over race. Since [moq-dev/moq#7652](https://github.com/moq-dev/moq/pull/7652) removed the records, each instance reads its media track and numbers its groups on the `media.join.interval` grid. A retired instance lists every group it read before the catalog update reached the export and stops at the first after; its replacement on the same track lists only groups past that boundary. The catalog and the media are still separate tracks, though, so the new config's first group can reach the old instance before the catalog update. The retiring instance claims it, so it is muxed under the old init and the replacement starts one group late. A catalog `start` field, a track per config, and a per-group marker were all dropped on 2026-11-02.

The nightly conformance test (`rs/moq-hls/tests/integration/conformance.rs`, from [moq-dev/moq#7387](https://github.com/moq-dev/moq/pull/7387)) passes its `video1` same-codec switch, but it neither forces nor observes which of the catalog update and the new config's first group reaches the export first. It is not a test of the race, so the fix needs its own test that forces that order.

Public API: none expected. Wire: decided by the removal plan.
