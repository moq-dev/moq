# [S] Track teardown on poll_unused is not atomic against a consumer reattaching

## Goal

Implement and verify the behavior tracked in [#3000](https://github.com/moq-dev/moq/issues/3000)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Found during the adversarial review of #2993, but pre-existing on `main` rather than introduced there.

`ietf::Subscriber::run_subscribe` treats an unused wake as an irrevocable terminal:

```rust
if track.poll_unused(waiter).is_ready() {
    return Poll::Ready(End::Unused);
}
```

followed by `track.abort(Error::Cancel)` and a cancellation upstream.

`poll_unused` is a level snapshot. A consumer can be obtained from the still-live cached track after the poll observes zero consumers and before `abort` runs, and that healthy consumer is then closed with `Cancel` while the upstream subscription is cancelled underneath it. Reordering the polls does not help; the gap is between observing the count and committing the teardown.

`js/net` partially guards the equivalent path by re-checking demand before breaking:

```ts
if (reason === idle && producer.closed.peek() === undefined && producer.used.peek()) continue;
```

Rust has no equivalent, and `track::Producer` exposes no `is_used()` to re-check with, so bringing it to parity means adding model API. The real fix is an atomic close-if-still-unused on the producer, coordinated with consumer creation, used by every unused-driven teardown. That is a model-layer change affecting the moq-lite path as well as the IETF one, which is why it was not folded into #2993.

Worth a regression test that interleaves demand returning after the unused wake but before teardown commits.

## Closes

- [#3000](https://github.com/moq-dev/moq/issues/3000) - close this issue when the quest finishes
