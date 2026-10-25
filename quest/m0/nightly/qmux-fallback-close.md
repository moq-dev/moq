# [S] qmux fallback closes cleanly

## Goal

Nightly's "WebSocket fallback startup benchmark" (`just js qmux-bench`, in
the Audio quality job) passes again instead of failing with
`WebSocketError: WebSocket was not cleanly closed`.

## Plan

New on 2026-10-24 and reproduced locally on `main`; it passed on 10-23.
Candidates in that window are
[moq-dev/moq#6409](https://github.com/moq-dev/moq/pull/6409) (qmux refuses
exhausted credit) and
[moq-dev/moq#6458](https://github.com/moq-dev/moq/pull/6458) (qmux abort and
close without the peer's help); the later qmux fixes
[moq-dev/moq#6644](https://github.com/moq-dev/moq/pull/6644) and
[moq-dev/moq#6687](https://github.com/moq-dev/moq/pull/6687) do not fix it.
Bisect, then fix the close at its cause.

Public API: none. Wire: none expected.
