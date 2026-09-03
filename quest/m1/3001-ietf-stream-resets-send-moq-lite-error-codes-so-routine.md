# [S] IETF stream resets send moq-lite error codes, so routine events read as INTERNAL_ERROR

## Goal

Implement and verify the behavior tracked in [#3001](https://github.com/moq-dev/moq/issues/3001)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Split out of #2993, where the cancellation path was fixed narrowly and the general case deliberately was not.

`Error::to_code()` encodes the moq-lite space, where `Cancel` is `0`. On the moq-transport wire, draft-19 section 3.3.4 assigns `0x0` to `INTERNAL_ERROR` and `0x1` to `CANCELLED`. Every IETF stream reset that goes through `Reader::abort`/`Writer::abort` therefore sends a code from the wrong registry.

The most visible case is `ietf/session.rs`, where a failed `run_uni_group` does `reader.abort(&err)`. A group arriving for an alias the subscriber retired by its own cancellation is an expected late object, and it stops the stream with `0` -- the publisher reads a routine event as an internal failure on our side.

\#2993 fixed exactly one site (`cancel_subscribe`, via a named `STREAM_CANCELLED` and a raw-code `Reader::stop`) because it could be shown safe there: the publisher treats its request stream reader closing as `Ok(())` and never inspects the code.

The general fix is not a matter of passing `1` at each remaining site, which is why it was not folded in:

- `Error::from_transport` maps only `0` back to `Cancel`. Changing outgoing codes without it makes two moq-net IETF peers disagree, so a routine cancellation surfaces as `Remote(1)` and can move an expected event into error paths.
- Both directions have to change together, and the mapping is shared with moq-lite, whose code space is unrelated.

So this wants a per-protocol code mapping (outgoing and incoming) rather than a scattering of literals. The js/net counterpart is #2999: there the abstraction cannot express a code at all.

## Closes

- [#3001](https://github.com/moq-dev/moq/issues/3001) - close this issue when the quest finishes
