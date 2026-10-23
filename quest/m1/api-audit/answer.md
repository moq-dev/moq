# [S] A request's source enforces its validity

## Goal

The source minted for a dynamic request refuses new track requests with
`Unroutable` once its route is replaced or its selection period retires, so
no session can serve a stale instance by forgetting a check. Tracks it
already accepted keep running and finish. Nothing becomes public.

## Plan

Decided 2026-10-23 (maintainer), reshaping
[moq-dev/moq#6411](https://github.com/moq-dev/moq/pull/6411) after the
public-model goal was dropped
([session-only items stay private](/quest/m1/api-audit/model-private.md)):

- Keep only the internal simplification: `broadcast::RequestValidity` and
  `origin::Selection` checks move into the source. The answering session
  learns of it when the source's request stream ends.
- Drop every planned public addition: `answer_source`, the unused-race
  family (`poll_unheld`, `close_unheld`, group `poll_unused` and
  `abort_unused`, `reject_unused`), `Producer::empty`/`Consumer::empty`,
  `Dynamic::withdrawn`, JS `Origin.Producer.fallback()`, and JS
  `Broadcast.requested` (deferred until a JS app needs it).
- Delete `wire.ts` `OriginConsumer.broadcasts`: registered, never read.

Public API: none. Wire: none.
