# [S] IETF peer FETCH refused old

## Goal

A cross-relay FETCH over a `moq-transport` peer link returns every group the origin still holds, instead of refusing some as `old`.
The cluster burst drill then runs a third peer link variant over `moq-transport-19`, and its impaired lane passes.

## Plan

Repro: in `rs/moq-relay/tests/drills.rs::cross_cluster`, pin the edge's peer link with `config.connect.version = ["moq-transport-19"]`.
The steady impaired drill then failed in 5 of 5 runs with FETCHes refused `old` for groups the origin still held, e.g. `groups lost: [(5, Failed("old")), (37, Failed("old")), (41, Failed("old"))]`.
Seeds: 11977074354257116273, 6755536760138253279, 17984197590923068922, 10974424043511566981.
The loopback lane and the flapping drill passed.

Find where the IETF FETCH path decides a group is `old`, fix it at the source, then add the IETF peer link as a `PeerLink` variant.

## Related

- [Cross-relay bursts re-run](/quest/m1/cross-relay-bursts.md) - the #4349 `Stream(Old)` stalls, not reproduced over a lite peer link
