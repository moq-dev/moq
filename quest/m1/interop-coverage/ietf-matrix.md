# [L] IETF interop matrix

## Goal

`just test interop` gains a moq-transport mode: every publisher and
subscriber pair it already runs on moq-lite (Rust, Python, Go, C++,
browser, Node, Bun, C, GStreamer) exchanges hang media through moq-relay
over each even draft from 14 to 22, and a subscriber decodes at least one
frame, nightly. A wire regression on any draft fails a cell.

## Plan

Decided with the maintainer on 2026-10-21: even drafts only, nightly; hang
media, no MSF.

- Pin the version per cell on both ends, the way the AUTH clients pin
  `moq-lite-07-wip` (`test/interop/interop.sh`), and assert the negotiated
  version from the logs so a silent fallback to lite fails the cell.
- Reuse the existing cases where they apply on IETF (the finite raw track,
  the CLI FETCH of group 0, clean close) rather than writing new ones.
- Recommendation: PRs keep the interop workflow's existing path filter and
  run draft 14 and draft 22 for Rust and JS only; the full even-draft matrix
  is nightly.
- Fix the test/interop README's claim that it covers IETF today.
- Report cells through the interop report's structured records
  (`test/lib/harness.sh`, rendered by `test/interop/report.py`), with the
  draft as the version field.

Public API: none. Wire: none.
