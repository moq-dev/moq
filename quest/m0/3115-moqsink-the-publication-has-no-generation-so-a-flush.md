# [M] moqsink: the publication has no generation, so a flush after EOS cannot restart it

## Goal

Implement and verify the behavior tracked in [#3115](https://github.com/moq-dev/moq/issues/3115)
within the issue's stated scope and boundaries.

## Plan

Rescoped during the 2026-08 grooming: items 2 and 3 landed on main via #2998
(Completion/CompletionHandle) and #3104. What remains is item 1: give the
publication a generation so an element-wide flush after EOS can restart it.

### Issue context

Split out of the adversarial reviews on #3101, #3102 and #3104. Each of those fixes a concrete `moqsink` lifecycle bug, and each one stops at the same wall: **a `moqsink` publication has no notion of a generation.** Once it ends, the only way back is a cycle through `READY`.

Three findings across those PRs are all the same root cause.

##### 1. A flushing seek after the element completed EOS is broken

Once the last pad sends EOS, `maybe_post_eos` finalizes every producer and takes the catalog. There is no way to reopen them. #3104 makes a post-EOS buffer answer `FlowError::Eos` instead of writing into finalized producers, which is an improvement over silently dropping it, but GStreamer specifies that `FLUSH_STOP` clears EOS and data flow resumes. We cannot honour that today.

A correct fix creates a new publication generation on a flushing restart (new broadcast/catalog/producers) rather than treating the first EOS as terminal for the element.

##### 2. `eos_posted` conflates two facts

It answers both "the producers were finalized" and "the EOS message was posted". #3104 gates the flush reset on it and therefore inherits the conflation. #2998 separates the two, which is what its per-pad lifecycle rewrite buys, but that separation does not reproduce standalone.

##### 3. The identity check and the bus post are not atomic

`post_session_error` (#3102) checks whether a session is still current, releases the element lock, then posts. A `PAUSED -> READY -> PAUSED` completing inside `post_message` still lands a stale error on the replacement's bus. The lock cannot be held across the post: `post_message` runs bus sync handlers inline on the calling thread, and a handler that reads an element or pad property would deadlock on it.

The natural remedy (an in-flight posting permit that teardown waits on) has the same problem in a different place: a sync handler calling `set_state(READY)` re-enters teardown on the thread already holding the permit. #2998 hits this too and accepts the window explicitly.

Deferring the post to a main-loop idle source would decouple it, at the cost of changing when the error is delivered and requiring a running main loop.

##### Why this is filed rather than fixed

Each PR is a strict improvement over `main` and none of them can close these without redesigning the publication lifecycle. That redesign overlaps heavily with #2998, so it should be settled once rather than three times.

@arielmol  -  flagging you since #2998 is the closest thing to a design for this, and its `Completion` state machine is most of the way to a generation. Worth deciding whether generations belong in that PR or in a follow-up on top of it.

## Closes

- [#3115](https://github.com/moq-dev/moq/issues/3115) - close this issue when the quest finishes
