# [S] A cancelled request releases its origin demand

## Goal

In `@moq/net`, a requester that leaves before a dynamic handler answers its
broadcast stops holding the origin's demand for it. Once the last requester
for a path leaves, that path's pending request is withdrawn, and its waiter
count and any stored rejection go with it. Other requesters for the same path
keep waiting. This covers lite SUBSCRIBE, FETCH, and TRACK info, and IETF
SUBSCRIBE.

## Plan

Found in #5214's review and deferred while #5314 rewrote the request path;
#5314 has merged.

The bug: `Consumer.#demand` in `js/net/src/origin.ts` enqueues the path on
its `ServeState`, bumps `demanding`, and waits on `settled` until the handler
answers, the route closes, or a reset releases it. Nothing ends that wait when
the requester leaves. The lite and IETF SUBSCRIBE paths race it with
`untilAborted`, which stops the caller waiting but leaves the promise running,
and lite FETCH and TRACK don't race it at all. So the `pending` request, its
queue entry, and the `demanding` count live until the handler answers, which
may be never. With a stalled handler, a peer that subscribes to distinct paths
and cancels each one grows that state without bound, while #5214's caps hand
the slots back.

`demand` takes an `AbortSignal` on the internal wire interface
(`js/net/src/wire.ts`). On abort the waiter drops its count. The request is
withdrawn the way `ServeState.reset` does, but for one path (out of `pending`
and the queue, finished so a late answer closes its front), only when nobody
else needs it. `demanding` counts wire waiters alone: a local
`Consumer.request()` reaches the same pending request through
`OriginState.route()`, which enqueues without counting, so the withdrawal
must also see that no local request or failover still routes to the path.
Callers pass the signal they already hold (`left.signal`, `hold`, or one tied
to the FETCH stream) instead of wrapping the call in `untilAborted`. Check
Rust's behavior (`Requests::remove_if` and `ServeState::forget` in
`rs/moq-net/src/model/origin.rs`) and match it, including whether a request a
handler has already taken is withdrawn.

Decided 2026-10-13: don't signal the handler that a request it took was
withdrawn. Its late answer just closes its front, and `Request` gets no new
public API; the bound doesn't depend on it.

Regression: a dynamic handler that never answers; for each covered request
(SUBSCRIBE in both protocol families, lite FETCH, and lite TRACK), request
many distinct paths and cancel each before an answer, then check that the
origin holds nothing for them. Also check that a second requester on a shared
path still resolves after the first cancels, both a second peer and a local
`Consumer.request()`, with the handler answering after the peer cancels.
Drive it on events, and use fake timers for any wait. Each case must fail on
main.

Public API: none, since `demand` is internal. Wire: none.

## Related

- [JS request deadline](/quest/m1/js-request-deadline.md) - once a request times out after 10 s, every unanswered one is cancelled, so each would hold demand without this
- [#5214](https://github.com/moq-dev/moq/pull/5214) - the JS session caps whose cancelled slots this finishes releasing
