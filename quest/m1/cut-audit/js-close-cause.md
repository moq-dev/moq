# [S] Settle received-track termination on a local session close

## Goal

Settle the received track's terminal state when the application deliberately
closes its session. State the transport, track, and unfinished-group outcomes
separately, then implement the chosen track behavior and finish this quest.

## Plan

The prepared implementation in
[moq-dev/moq#6978](https://github.com/moq-dev/moq/pull/6978) fixes the explicit
plan's three reproducible paths: unfinished groups are cancelled while
finished groups stay readable, lite protocol violations retain their error
through teardown, and an unanswered request is refused with an error rather
than inventing an empty finished track. `Request.reject` now requires its
error. Rust's `Error::Dropped` maps to INTERNAL_ERROR, which the pending
request refusal uses.

A remaining conflict in the previous wording needs the maintainer's choice:
its Goal required local session close to error the received track, but its
Plan explicitly kept public `abort()` clean. The implementation follows the
explicit Plan and current Rust behavior:

- Transport: the application's immediate close uses the clean session code.
- Received track: its local route closes without declaring a content end.
- Unfinished group: it ends with CANCELLED; a group the publisher finished
  stays readable.

Choose whether to retain that clean local route-close behavior or abort the
received track with CANCELLED as well. Recommend retaining the mirrored Rust
behavior, with cancellation at the unfinished-group boundary. No broader
session redesign is included; the independent FIN-and-peer-close ordering
work lands in [moq-dev/moq#6969](https://github.com/moq-dev/moq/pull/6969).

Public API: breaking in the prepared PR. Callers pass an error to
`Request.reject`, and finish groups before closing when they need a complete
group. Wire: unanswered requests reset with INTERNAL_ERROR instead of ending
as empty finished tracks.
