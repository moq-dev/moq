# [L] Align origin broadcast creation naming across language bindings

## Goal

Implement and verify the behavior tracked in [#3190](https://github.com/moq-dev/moq/issues/3190)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

The operation that allocates and owns a new broadcast has different names across public surfaces:

- Rust FFI uses [`create_broadcast`](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/rs/moq-ffi/src/origin.rs#L251-L267)
- Python, Swift, and Go expose `create_broadcast` or `CreateBroadcast`, for example [Go](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/go/wrapper/origin.go#L38-L51)
- TypeScript uses [`publish(path)`](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/js/net/src/origin.ts#L106-L107)
- C uses [`moq_origin_publish`](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/rs/libmoq/src/api.rs#L1102-L1123)

`publish` suggests announcement or transmission. The operation actually creates a broadcast, while announcement visibility is controlled separately through `setAnnounce` or its equivalent.

Issue #1073 changes Origin lifecycle but explicitly leaves naming outside its scope.

#### Proposed direction

Use the role-accurate creation verb consistently:

- Rust and Python: `create_broadcast`
- TypeScript, Swift, and Kotlin: `createBroadcast`
- Go: `CreateBroadcast`
- C: `moq_origin_create_broadcast`

If compatibility aliases are retained, keep deprecated names hidden from user-facing documentation according to the repository deprecation policy.

#### Acceptance criteria

- Every language uses its idiomatic spelling of the same `create broadcast` operation.
- Announcement APIs retain separate announce terminology.
- Examples and generated documentation use only the canonical name.
- Breaking renames target `dev`.

## Closes

- [#3190](https://github.com/moq-dev/moq/issues/3190) - close this issue when the quest finishes
