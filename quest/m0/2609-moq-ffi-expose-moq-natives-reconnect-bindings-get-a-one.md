# [M] moq-ffi: expose moq-native's reconnect - bindings get a one-shot session that stalls silently

## Goal

Implement and verify the behavior tracked in [#2609](https://github.com/moq-dev/moq/issues/2609)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### What

`moq-ffi` dials once and never reconnects. `Client::connect` in `rs/moq-ffi/src/session.rs` does:

```rust
let session = client
    .with_publisher(&publish)
    .with_subscriber(subscribe.clone())
    .connect(url)
    .await
    .map_err(map_connect_error)?;
```

Meanwhile `moq-native` already has the machinery  -  `Client::reconnect(url)` (background loop, exponential backoff), plus `Client::publish` / `Client::consume`, and `consume` applies `origin.with_linger(...)` so broadcasts survive the gap. None of it is reachable from the FFI: `grep -rn "reconnect\|Reconnect" rs/moq-ffi/src/` returns nothing, and the Python `moq.Client` constructor exposes no such option.

#### Why this can't be worked around above the FFI

The obvious workaround  -  wrap the session in a retry loop in the binding language  -  doesn't work, because the failure that actually matters is a **silent stall**, not an error.

Observed in a Python service that watches a prefix for announcements:

- It ran `async for announcement in origin.consume().announced(prefix)` inside `async with moq.Client(...)`, wrapped in a `while True` that redials on exception or normal exit.
- The host laptop slept, severing the QUIC path.
- The iterator **never yielded, never returned, and never raised**  -  for 2h45m. Zero redial attempts, because there was nothing to react to.
- Peers went on announcing under that prefix the whole time (confirmed from a browser on the same relay, which saw them). The service just never learned.
- From the outside it looked healthy: process up, HTTP served, no errors logged.

A retry loop above the FFI can only catch "ended or raised". Noticing a path that has gone quiet needs keepalive/timeout state inside the session  -  which is exactly where `Reconnect` already lives.

For contrast, the JS side has `Connection.Reload` and rides out the same drops fine; the Rust side has `Client::reconnect`. The FFI is the odd one out, so Python/Kotlin/Swift consumers each end up hand-rolling something that structurally cannot work.

#### Suggested shape

Expose the reconnect path rather than the one-shot dial  -  roughly `Client::consume` / `Client::publish` semantics, returning something that keeps the origin alive across drops, with `Backoff` (`initial` / `multiplier` / `linger`) configurable through the FFI config. A `closed`-style handle would let callers distinguish "still retrying" from "gave up", which the one-shot API can't express either.

Happy to take a pass at the binding if the shape is agreed.

#### Pointers

- `rs/moq-ffi/src/session.rs`  -  `Client::connect`, the one-shot dial
- `rs/moq-native/src/client.rs`  -  `reconnect()`, `publish()`, `consume()`
- `rs/moq-native/src/reconnect.rs`  -  `Backoff`

## Closes

- [#2609](https://github.com/moq-dev/moq/issues/2609) - close this issue when the quest finishes
