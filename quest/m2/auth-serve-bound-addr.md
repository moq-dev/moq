# [XS] Auth serve logs the address it bound

## Goal

`moq auth serve --listen 127.0.0.1:0` logs the port the OS actually assigned,
not port 0, so scripts and tests can find the server.

## Plan

- In `Serve::run` (`rs/moq-cli/src/auth.rs`), log `listener.local_addr()`
  after binding instead of the configured `addr`.
- Found while abandoning `quest/m1/test-flakes-2/auth-serve-signals.md`
  (#5707).
