# [S] Make every blocking Go operation cancellable with context.Context

## Goal

Implement and verify the behavior tracked in [#3188](https://github.com/moq-dev/moq/issues/3188)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

The Go package contract says blocking calls accept `context.Context` so callers can cancel them:

- [Package concurrency documentation](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/go/wrapper/doc.go#L14-L27)

Several operations backed by async UniFFI methods are nevertheless exposed as contextless, synchronous Go calls:

- [`OriginConsumer.RequestBroadcast`](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/go/wrapper/origin.go#L126-L136)
- [catalog, track, fetch, resolve, and decode operations](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/go/wrapper/subscribe.go#L43-L130)
- [JSON subscriptions](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/go/wrapper/json.go#L160-L184)

These can wait on remote producer behavior or network progress without any caller-controlled deadline.

#### Root cause

The generated async UniFFI calls block the calling Go goroutine. The handwritten wrapper applies cancellation adapters to only part of that surface, so the documented rule and actual API have drifted apart.

#### Proposed direction

Make every potentially blocking public Go operation accept `context.Context`.

Prefer cancellable FFI operation handles for long-running construction and resolution. Wrapping the current synchronous call in an abandoned goroutine returns control to Go but leaves the native operation alive until it eventually completes.

#### Acceptance criteria

- Every potentially blocking Go method accepts a context.
- Cancellation and deadlines return promptly.
- Cancellation does not leak an unbounded native task or goroutine.
- Package documentation accurately describes any remaining cancellation limitations.
- Tests cover cancellation while waiting for a dynamic broadcast and track resolution.
- Breaking signature changes target `dev`.

## Closes

- [#3188](https://github.com/moq-dev/moq/issues/3188) - close this issue when the quest finishes
