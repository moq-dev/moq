# [M] A track judges a passed group from its arrivals, not its newest sequence

## Goal

When a route fails over, a group whose stream header arrives after its
successor's still continues on the new route. Only a group the publisher
dropped, or one whose gap outlived its grace, ends with an error. Held fetches
over a pre-lite-05 upstream (#5209) and route resume use the same rule.

## Plan

Found by Codex on #5209 (2026-10-10). `track::Consumer::poll_group(seq)`
answers that the feed went past `seq` as soon as any newer group arrived
(`max_sequence > seq`). Each group is its own QUIC stream, so N+1's header can
beat N's. Resume (`rs/moq-net/src/model/resume.rs`, the refusal verdict) then
fails an in-flight group it could have continued.

- Decided: build on [Tail arrivals](/quest/m1/tail-arrivals.md), the per-track
  record of live arrivals in the model, and make `poll_group`'s "passed" verdict
  read it. A gap stays open for the subscription's grace, as `tail::Tail` does
  in the lite subscriber today.
- Decided: #5209's held fetches move off their local `Tail` read onto this
  verdict, so one rule remains.
- The delivery budget alone was rejected: relay tracks from lite-01..04
  upstreams are untimed, so nothing on them is ever stale.
- Tests use mocked time: a failover mid-group where N+1's header arrives before
  N's continues N, and a truly dropped group still fails once its gap folds.

Public API: behavior only. Wire: none.

## Required

- [Tail arrivals](/quest/m1/tail-arrivals.md) - the per-track arrival record this verdict reads
