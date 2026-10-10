# [M] moq-uring: write the WebTransport stream header at open time, so finish() never owes one

## Goal

A web-mode `SendStream` (`rs/moq-uring/src/quic/web.rs`) has its WebTransport
header on the wire before `poll_open_uni` hands it back, so `finish()` never
owes one and the `finishing` state machine goes away. Concurrent openers,
cancellation, and flow-control backpressure on the open have settled,
documented semantics.

## Plan

Follow-up to #3105 (item 4).

##### Where it stands

A web-mode `SendStream` carries its WebTransport header as an owed `prefix` and writes it lazily on the first `poll_write`. When `finish()` finds no credit for the header, it records `finishing` and returns `Ok`, and the FIN goes out on a later `poll_closed`, or on `Drop` if credit has returned by then.

That covers every in-tree caller, because moq-net always pairs `finish()` with `poll_closed`. What it does not cover is a direct consumer that finishes and drops in the same breath:

```rust
send.finish()?;   // no credit: the header is owed, returns Ok
drop(send);       // no ingress in between, so try_write still returns 0
```

There is no moment for credit to return between those two calls, so `Drop` falls through to the mapped reset and the peer sees a cancellation despite `finish()` having reported success. `SendStream` documents this, but it is still a wart.

##### The fix

Option 1 from #3105: queue the prefix before handing the opened stream back, so nothing is ever owed at finish time. A WebTransport stream is arguably not open until its header is on the wire, so this is also the more honest shape.

It is not a small change: `poll_open_uni` would have to hold a half-open stream across polls while the header drains, and `Session` clones share one `Rc<Web>`. A single slot for the in-flight open would have concurrent openers fighting over it, and a queue alone is not a chosen contract. Settle the contract first:

- How independent concurrent openers keep ownership of a half-open stream across `Pending` and cancellation (a dropped open future).
- Whether an open-before-read caller can deadlock when the header needs flow-control credit. Making `open` block on credit moves backpressure earlier, which is more correct but changes when a caller learns about it.
- Resource bounds on half-open streams.

Present the viable ownership choices and a recommendation to the maintainer before implementing. Regression cases: concurrent openers, credit starvation, dropped open futures, and finish then drop.

Decided 2026-10-08: the open-contract planning quest merged in here, and the quest moved to m3. No in-tree caller hits the wart and io_uring ships in no package.

## Closes

- [#3129](https://github.com/moq-dev/moq/issues/3129) - close this issue when the quest finishes
