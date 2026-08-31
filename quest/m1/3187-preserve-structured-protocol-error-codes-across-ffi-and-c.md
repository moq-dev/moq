# [M] Preserve structured protocol error codes across FFI and C bindings

## Goal

Implement and verify the behavior tracked in [#3187](https://github.com/moq-dev/moq/issues/3187)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

Rust preserves structured session and stream failures, including known protocol variants, application codes, and unknown future codes:

- [`SessionError::App(u16)` and `Unknown(u32)`](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/rs/moq-net/src/error.rs#L60-L108)
- [TypeScript session and stream code types](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/js/net/src/error.ts#L10-L145)

The UniFFI boundary flattens every `moq_net::Error` into the broad `MoqError::Protocol` variant:

- [FFI error mapping](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/rs/moq-ffi/src/error.rs#L1-L8)

The C facade similarly reports a broad status with textual detail. Python, Swift, Kotlin, Go, and C callers can send application error codes through abort and cancel APIs, but cannot reliably inspect a received code. This prevents applications from implementing protocol-defined recovery or policy.

#### Proposed direction

Expose a portable structured protocol error shape that preserves:

- session versus stream scope
- the exact numeric code
- a known semantic kind when recognized
- unknown future codes without lossy conversion
- a human-readable message for diagnostics

Keep transport and internal failures separate from protocol failures. C should expose equivalent getters or an output record rather than requiring callers to parse `moq_error()`.

#### Acceptance criteria

- Every binding can recover the exact received session or stream code.
- Application-defined and unknown codes round-trip without loss.
- Callers can still match broad error categories ergonomically.
- Cross-language tests cover a known code, an application code, and an unknown code.
- The public shape is designed on `dev` before the next binding compatibility release.

## Closes

- [#3187](https://github.com/moq-dev/moq/issues/3187) - close this issue when the quest finishes
