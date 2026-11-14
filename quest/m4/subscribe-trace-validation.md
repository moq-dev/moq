# [L] Subscription runs are checked against the TLA+ model

## Goal

A seeded sweep of one lite-07 subscription, publisher and subscriber over the
in-memory transport, records every run, and each is also a behaviour of
`SubscribeAccounting.tla`: a change to drops, skips, updates, or group stream
handling that the model does not allow fails the sweep, without anyone
remembering to resync the model.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan on what could be proved about
moq-net): every TLA+ model gets trace validation, not route alone. Milestone
m4, behind route's.

- Reuse the trace format and the TLC recipe
  [route trace validation](/quest/m3/route-trace-validation.md) builds, with
  the `verify-subscribe` skill's Coverage table as the mapping from code to
  model actions.
- The model's steps are spread over `lite/publisher.rs` (`TrackRun`,
  `GroupServe`), `lite/subscriber.rs` (`ServeLoop`, `TrackEntry`), and
  `tail.rs`, on two sides of a session. Record both sides into one trace
  ordered by the mocked clock and the transport's delivery order.
- No seeded sweep of this path exists. Add one that draws group arrivals,
  give-ups, updates, resets, expiry, and the order the Subscribe Stream and
  Group Streams are delivered in, following the repository's seed convention,
  and check the skill's rules on each run directly too.
- [Shared group serving and ingest](/quest/m4/shared-serve.md) moves the
  serving loop both protocols share. Whichever lands second re-maps the
  Coverage table.

Public API: none. Wire: none.

## Required

- [Route sweeps are checked against the TLA+ model](/quest/m3/route-trace-validation.md) - the trace format and TLC recipe this reuses

## Related

- [Shared group serving and ingest](/quest/m4/shared-serve.md) - moves the code this validates
