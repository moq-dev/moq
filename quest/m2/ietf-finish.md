# [M] moq-transport finish interop

## Goal

The interop `finish` case passes on every even moq-transport draft, not only
moq-lite: each reader that joins a finished broadcast while its publisher retains it
(moq-mux's fixed linger) gets the whole ten-frame tail, natural media and catalog EOF, the exact broadcast's
withdrawal, and the publisher's later health broadcast through the same
session. Until then `interop.sh` records those cells as skipped; delete that
skip in `run_finishes` and its README note.

## Plan

Observed on 2026-10-22 with `just test interop --finish --all --versions ietf`
(lite passes 45/45):

- Draft 18: late readers see the finished catalog end empty, a short
  tail, or a refused joining FETCH (`publish done: internal error`). This is
  the pre-draft-22 join after a finish, which
  [join after a quick finish](/quest/m2/ietf-join-after-finish.md) fixes; rerun
  these drafts once it lands.
- Draft 22: several readers time out after the withdrawal (C and C++
  on the health broadcast), JS readers see `publish done: internal error`, and
  native JS refuses a "malformed request stream message" from the relay.
  Reproduce each between two clients before fixing; they may be separate bugs.

A failing reader fails its whole publisher row, since the publisher waits for
every reader's acknowledgement, so judge each cell from its reader's log.

Public API: none. Wire: none expected; any fix that changes framing updates
its draft.

## Required

- [Join after a quick finish](/quest/m2/ietf-join-after-finish.md) - the pre-draft-22 half of this, without which draft 18 cannot pass
