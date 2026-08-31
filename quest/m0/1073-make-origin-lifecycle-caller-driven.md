# [M] Make Origin lifecycle caller-driven

## Goal

Implement and verify the behavior tracked in [#1073](https://github.com/moq-dev/moq/issues/1073)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Summary

Make Origin lifecycle caller-driven. Target `dev` because this intentionally breaks the published Rust API.

Closed PR #2472 is prototype evidence, not an accepted implementation. This work is a prerequisite for the runtime-neutral goals in #2875.

#### Public API

- Make `origin::Producer::new(info) -> (Producer, origin::Driver)` the only public factory.
- Remove `Origin::produce()` and `origin::Info::produce()`.
- Expose `origin::Driver` as a `#[must_use]` future with `Output = ()` and a `poll(&Waiter)` API matching the session driver.
- Add `moq_native::origin::spawn(info) -> Producer`, which constructs the pair and spawns the driver with Tokio. Calling it outside a Tokio runtime may panic.
- Defer an explicit `origin::Producer::abort(err)` API.
- Keep `web_async::time` timers. Runtime-independent time remains part of #2875.

#### Lifecycle behavior

- Replace origin-internal `web_async::spawn` calls with one driver-owned task set for source watchers, fronts, and track-serving tasks.
- Perform eligible initial attachment, exact-path visibility, and announcement updates synchronously in `create_broadcast`.
- Pass the already-attached state into the queued watcher so the first driver poll does not attach twice.
- Require driver polling for route changes, track serving, linger timers, failover, and teardown.
- The driver must hold no producer clone and finish after producers and submitted lifecycle work drain.
- Dropping the driver cancels its work, immediately tears down the origin with `Error::Dropped`, rejects pending requests, unannounces active broadcasts, and ends announcement cursors.
- Producer mutations after driver drop return `Error::Closed`.

Immediate visibility deliberately preserves the current publication-readiness race. #2895 tracks an atomic readiness gate separately and does not block this issue.

#### Migration

- Migrate native applications, relay components, and libmoq to `moq_native::origin::spawn`.
- Have `moq-wasm` spawn the returned driver with `web_async::spawn`.
- Make direct `moq-net` users, tests, and examples retain and explicitly poll or spawn the driver.
- Preserve all wire behavior, JavaScript APIs, and public FFI signatures.
- Do not update drafts or bump package versions.
- Update Rust API documentation and examples to show construction and the driver lifetime contract.

#### Acceptance

- Exact lookup and eligible announcement state update synchronously without polling the driver.
- Route changes, track serving, and nested lifecycle work make no progress until the driver is polled.
- Immediate and parked initial attachment do not duplicate sources or announcements.
- Driver drop aborts active fronts with `Dropped`, rejects pending dynamic requests, emits unannounces, closes cursors, and makes later producer mutation fail with `Closed`.
- The driver naturally finishes without retaining the origin.
- `moq_native::origin::spawn` covers progression, teardown, and its outside-runtime panic.
- `nix develop --command just fix`, `just check`, and `just test` pass.
- Run `just test smoke-full` if the libmoq adaptation changes the exercised C/FFI path.

#### Out of scope

- The crate rename in #2875.
- Directional producer/consumer naming.
- Lookup ergonomics.
- Timer abstraction.
- Explicit custom-error abort.
- The atomic readiness gate in #2895.

## Closes

- [#1073](https://github.com/moq-dev/moq/issues/1073) - close this issue when the quest finishes

## Related

- [#2895: Add an atomic readiness gate for Origin broadcasts](/quest/m0/2895-add-an-atomic-readiness-gate-for-origin-broadcasts.md) - related open work
