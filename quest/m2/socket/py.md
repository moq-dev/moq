# [S] Python sockets

## Goal

moq-ffi exposes `Listener`, `dial`, and `Socket` beside origin, and
`py/moq-rs` wraps them, so a Python listener accepts a Rust dialer's socket
and the reverse, with uni and bi streams each way.

## Plan

Bind the [Rust surface](/quest/m2/socket/rust.md) through moq-ffi the way
origin's request and announce APIs are bound today (`rs/moq-ffi/src/origin.rs`,
`py/moq-rs/moq/origin.py`). Add a Python/Rust interop test, and document the
API wherever `py/moq-rs` documents origin.

## Required

- [Rust sockets](/quest/m2/socket/rust.md) - the surface this binds
