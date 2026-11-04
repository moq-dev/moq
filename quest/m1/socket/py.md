# [S] Python sockets

## Goal

moq-ffi exposes the socket `Endpoint` and `Stream` beside origin, and
`py/moq-rs` wraps them, so a Python endpoint accepts a Rust endpoint's dial and
the reverse, carrying messages both ways.

## Plan

Bind the Rust surface (`moq_net::socket`): `Endpoint::new`, `dial`, `accept`,
an incoming dial's `accept` and `reject`, and `Stream`'s `producer`,
`consumer`, `peer`, and `session`, through moq-ffi the way origin's request
and announce APIs are bound today (`rs/moq-ffi/src/origin.rs`,
`py/moq-rs/moq/origin.py`). Add a Python/Rust interop test, and document the
API wherever `py/moq-rs` documents origin.
