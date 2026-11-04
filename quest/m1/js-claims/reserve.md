# [S] Re-serving a claimed path keeps the session

## Goal

A JS publisher that answers a claimed path's request with a fresh broadcast
whose first group is above 0 (continuing an earlier broadcast's group
sequence) keeps its session through a relay on `moq-lite-07-wip`; today the
relay closes the publisher's connection with `invalid control encoding`.

## Plan

Seen 2026-11-03 in Kixel's T3 tracks
([kixelated/t3code#1](https://github.com/kixelated/t3code/pull/1)), which
worked around it by replacing tracks in the same broadcast instead. Find
which side encodes or decodes the start group wrongly, fix it, and add a
relay test that re-serves a path starting at group 1.

On 2026-11-04, the candidate test in `test/interop/claims.ts` passed against
`main` at `ffcb7cd9748422daa30e935ec464108b5a8ebb71`, before any encoder or
decoder change. It uses a real `moq-relay` on lite-07, keeps the publisher
connection and claim instance, closes the first broadcast and track, opens a
fresh reader connection, and answers the same path with a fresh broadcast
whose first group is 1. Groups begin only after upstream demand. It uses the
reporting app's relative-zero range, ascending order, unlimited subscriber
delay and publisher retention, refresh 1, and publisher priority 1.

Additional local variants also passed: first answer at group 1, groups buffered
before serving, an absolute group-1 subscription floor, and a fresh route
instance between answers. The control codecs have not been changed, because
none of these cases reproduces the reported invalid encoding.

## Waiting

- Maintainer: provide the failing sequence or paired JS/relay revisions so the
  control-encoding failure can be reproduced (recommended), or accept the
  regression test as evidence that this quest can be retired. The original
  report does not give a minimal failing sequence, and the candidate passes
  unchanged on current `main`.
