# Go bindings for MoQ

Go bindings for [MoQ (Media over QUIC)](https://github.com/moq-dev/moq).

Two packages:

- [`moq-lite/`](./moq-lite/) — the ergonomic wrapper. **Use this.**
- [`moq-ffi/`](./moq-ffi/) — raw UniFFI-generated bindings. Implementation
  detail; users should not import this directly.

See [`moq-lite/README.md`](./moq-lite/README.md) for quick-start and the
expected build flow (`just go build`).

## Modules

This directory is two separate Go modules (one per subdirectory) tied
together by a workspace [`go.work`](../go.work) at the repo root so
contributors can develop both without an intervening release.

Published module paths once tagged:

- `github.com/moq-dev/moq/go/moq-lite`
- `github.com/moq-dev/moq/go/moq-ffi`
