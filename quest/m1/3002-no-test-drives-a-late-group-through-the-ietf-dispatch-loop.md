# [S] No test drives a late group through the IETF dispatch loop

## Goal

Implement and verify the behavior tracked in [#3002](https://github.com/moq-dev/moq/issues/3002)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Raised by the adversarial review on #2993.

`ietf::error::to_stream_code` decides the code a failed uni stream is stopped with, and `run_unis` in `ietf/session.rs` is the one line that applies it:

```rust
reader.stop(super::error::to_stream_code(&err));
```

Nothing exercises that line. The regression added in #2993 (`a_retired_alias_maps_to_the_cancelled_code`) proves the alias resolves to `Error::Cancel` and that `Cancel` maps to `CANCELLED`, but it never owns a receive stream and never observes `Log::stops()`. It would still pass if the dispatch loop stopped calling the mapper, or stopped calling `stop` at all.

Driving it end to end needs two things that put it beyond the scope of that PR:

- A test session whose `accept_uni` yields a scripted stream. `ScriptedSession::accept_uni` parks and `DeadStreamSession` hands out an already-dead stream, so neither can deliver a SUBGROUP\_HEADER.
- Reach across a module boundary. `run_unis` is private to `session`, while retiring an alias touches state private to `subscriber`, so no single test module can set up the state and drive the loop.

The shape worth having: script one uni stream carrying a subgroup header for a retired alias, run the dispatch loop over it, and assert the transport recorded a STOP\_SENDING of `0x1`. The uni-script support would be reusable for any other test that wants to drive that loop, of which there are currently none.

## Closes

- [#3002](https://github.com/moq-dev/moq/issues/3002) - close this issue when the quest finishes
