# [S] Server cancel releases the listener

## Goal

`MoqServer.cancel` returns only once the listening socket is closed, so a
caller can bind the same address immediately afterwards without retrying.
Today it signals the accept task and returns; the socket closes when that
task observes the cancellation, so callers race their own teardown. The Go
reconnect test works around it by retrying the bind.

## Plan

Found while writing the reconnect coverage in
[#3627](https://github.com/moq-dev/moq/pull/3627): `go/wrapper/reconnect_test.go`
restarts a relay mid-run through `Server.Close`, which is `MoqServer.cancel`
in `go/wrapper/server.go`, and has to retry the bind over the async teardown
of the previous instance. The doc comment on `cancel` in
`rs/moq-ffi/src/server.rs` already promises that the socket is closed there,
not when the handle drops; the implementation only cancels the task.

The surface is the one UniFFI method. Every binding reaches it under its own
name: Go `Server.Close`, Swift `cancel`, Kotlin `close`, Python only through
`Server.__aexit__` (the public wrapper exposes no `cancel`; the generated
`moq_ffi.MoqServer` does), Dart through the generated `MoqServer`. None
needs a new operation, only the existing one to wait, and the regression per
binding drives the entry point its callers actually use.

Open questions:

- Where the wait lives: `MoqServer.cancel` joining its `Task`, or
  `moq-tokio`'s server exposing a closed future the FFI awaits.
- Whether blocking on the join is safe from the threads the FFI runtime calls
  `cancel` on, or whether the socket must be closed synchronously before the
  task is told, so no join is needed.

The fix removes the retry from the Go test; it does not add one to the
library, and it changes no binding's API.
