# [M] Expose a cancellable route watch API in Python

## Goal

Implement and verify the behavior tracked in [#3193](https://github.com/moq-dev/moq/issues/3193)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

Python handles route changes differently from the other bindings. `BroadcastConsumer.route_changed()` lazily creates a route watch, caches it in a private field, and awaits its next value:

- [Python `BroadcastConsumer` route handling](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/py/moq-rs/moq/subscribe.py#L376-L405)

The watch handle is not public and has no deterministic cancellation path. A caller that abandons route observation must rely on garbage collection or broadcast shutdown to release the native watch.

Other wrappers expose explicit lifetime ownership:

- [Go `RouteWatch.Next(ctx)` and `Cancel`](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/go/wrapper/subscribe.go#L15-L40)
- Swift exposes a cancellable asynchronous sequence.
- Kotlin exposes a `Flow` that cancels the watch in `finally`.

#### Proposed direction

Expose a public Python route watch abstraction with deterministic cleanup, following the existing wrapper conventions. Suitable shapes include:

- an async iterator returned by `routes()`
- an async context-managed `RouteWatch`
- both, with `route_changed()` retained only as a convenience over the owned watch

The owner should be able to cancel a pending wait and release the native resource without waiting for garbage collection.

#### Acceptance criteria

- Python callers can explicitly own and close a route watch.
- Abandoning iteration deterministically cancels the native watch.
- A pending route wait is cancellable by normal asyncio task cancellation.
- Tests cover early iterator exit and cancellation while waiting.
- Lifecycle documentation matches the Go, Swift, and Kotlin ownership contract.

## Closes

- [#3193](https://github.com/moq-dev/moq/issues/3193) - close this issue when the quest finishes
