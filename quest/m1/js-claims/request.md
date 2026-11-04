# [S] A claimed path resolves through origin.request()

## Goal

Through a relay on `moq-lite-07-wip`, a JS reader's `origin.request(path)`
for a path a JS publisher claimed and answers on demand resolves to the
served broadcast, as `origin.follow(path)` already does.

## Plan

Seen 2026-11-03 in Kixel's T3 tracks
([kixelated/t3code#1](https://github.com/kixelated/t3code/pull/1)): `follow()`
worked and `request()` never resolved. Reproduce with a JS publisher and a
JS reader through a `moq-relay` on lite-07, find whether the relay or the JS
origin drops the request, fix it, and keep the test.

Investigated 2026-11-04 against `main` at `ffcb7cd974`: a JS publisher
answering `origin.requests()` with `request.serve()` and a JS reader through
a real `moq-relay` on lite-07 resolve both the claim's own path and a child
path. The own-path case also passes with an asynchronous answer under Node
and Bun, over WebSocket and native WebTransport. No source failure has been
reproduced.

A reader reaching the relay before the publisher's claim is advertised gets
`Unroutable` after its initial discovery syncs empty. Starting the publisher
then resolves a standing follower while that request remains terminal, as
`request()` documents. The original consumer now explicitly uses `follow()`
to wait for that ordering. This is a possible explanation, not a confirmed
reproduction of the original report.

## Waiting

- Maintainer: choose whether to retire this quest as unreproduced on current
  `main` (recommended), or provide the failing revision and sequence so its
  remaining reproduction can be isolated. Changing `request()` to wait after
  an empty discovery result would change the settled request/follow contract;
  no API or wire change is justified by the current evidence. The draft PR
  records the runnable reproduction and checks; no production fix has been
  made.
