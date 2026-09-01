# [M] moq-uring: write the WebTransport stream header at open time, so finish() never owes one

## Goal

Implement and verify the behavior tracked in [#3129](https://github.com/moq-dev/moq/issues/3129)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Follow-up to #3105 (item 4), deliberately left out of #3110. Raised by Codex reviewing that PR and verified against the code.

##### Where it stands

A web-mode `SendStream` carries its WebTransport header as an owed `prefix` and writes it lazily on the first `poll_write`. #3110 stopped `finish()` from reporting connection-level backpressure as a terminal `Error::Web`: it now records that the header is still owed and returns `Ok`, and the FIN goes out on a later `poll_closed`, or on `Drop` if credit has returned by then.

That covers every in-tree caller, because moq-net always pairs `finish()` with `poll_closed`. What it does not cover is a direct consumer that finishes and drops in the same breath:

```rust
send.finish()?;   // no credit: the header is owed, returns Ok
drop(send);       // no ingress in between, so try_write still returns 0
```

There is no moment for credit to return between those two calls, so `Drop` falls through to the mapped reset and the peer sees a cancellation despite `finish()` having reported success. This is not a regression (before #3110 the same situation returned `Err` and callers dropped, resetting with a worse code), and #3110 documents it on `SendStream`, but `finish()` reporting success on a stream that ends up cancelled is still a wart.

##### The fix

Option 1 from #3105: queue the prefix before handing the opened stream back, so nothing is ever owed at finish time and the whole `finishing` state machine goes away. A WebTransport stream is arguably not open until its header is on the wire, so this is also the more honest shape.

The reason it is not a small change: `poll_open_uni` would have to hold a half-open stream across polls while the header drains, and `Session` clones share one `Rc<Web>`. A single slot for the in-flight open would have concurrent openers fighting over it, so this needs a queue of half-open streams in `Web::state`, plus a decision about what `poll_open_uni` returning `Pending` on flow control means for callers that open before they read.

Worth confirming the trade too: making `open` block on credit moves the backpressure earlier, which is more correct but changes when a caller learns about it.

## Closes

- [#3129](https://github.com/moq-dev/moq/issues/3129) - close this issue when the quest finishes
