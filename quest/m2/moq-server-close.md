# [S] Synchronous server close

## Goal

A caller that closes a `MoqServer` can bind the same port immediately after
`Close` returns, without retrying. Today close returns before the underlying
listener is released, so callers race their own teardown; the Go reconnect test
works around it by retrying the bind.

## Plan

Found while writing the reconnect coverage in
[#3627](https://github.com/moq-dev/moq/pull/3627): the Go test restarts a
relay mid-run and has to retry the bind over the async teardown of the previous
instance.

Open questions:

- Which layer owns the join: `moq-ffi`'s `MoqServer`, `moq-tokio`'s server, or
  the generated bindings' runtime?
- Can `Close` block on the accept loop's task join without deadlocking on the
  FFI runtime's threads?
- Is a synchronous `Close` the right contract for every generated binding, or
  should release be an explicit awaitable step? UniFFI's surface shapes what
  callers can express.

No direction chosen. The fix removes the retry from the Go test; it does not
add one to the library.
