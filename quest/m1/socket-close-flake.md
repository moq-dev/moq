# [S] A socket endpoint's close always reads as Closed

## Goal

`py/moq-rs/tests/test_socket.py::test_abort_and_endpoint_close_are_errors` passes every run: a read on a session whose peer endpoint closed fails with `Closed`, never with `Protocol: unroutable`.

## Plan

Seen 2026-11-15 in one `just check` run on the branch of [moq-dev/moq#8817](https://github.com/moq-dev/moq/pull/8817): after `a.close()`, `server.consumer.read_frame()` raised `MoqError.Protocol: unroutable` instead of `Closed`; a rerun of `py` `just test` passed.
The socket model (`rs/moq-net/src/model/socket.rs`, `rs/moq-ffi/src/socket.rs`) does not use the reconnect policy that PR changed, so the race is older.
Reproduce it in a Rust test of the socket model, likely an endpoint's close racing the read's route lookup, and fix the root cause rather than loosening the assertion.
