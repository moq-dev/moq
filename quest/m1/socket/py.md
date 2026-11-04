# [S] Python sockets

## Goal

moq-ffi exposes the socket `Endpoint`, `TcpStream`, and `TrackStream` beside
origin, and `py/moq-rs` wraps them, so a Python endpoint accepts a Rust
endpoint's dial and the reverse, in both protocols, carrying data both ways.

## Plan

Bind the Rust surface (`moq_net::socket`): `Endpoint::new`, `dial_tcp`,
`accept_tcp`, `dial_track`, `accept_track`, an incoming dial's `accept` and
`reject`, and each stream's `producer`, `consumer`, `peer`, and `session`
(group handles for `TcpStream`, track handles for `TrackStream`), through
moq-ffi the way origin's request
and announce APIs are bound today (`rs/moq-ffi/src/origin.rs`,
`py/moq-rs/moq/origin.py`). Add a Python/Rust interop test, and document the
API wherever `py/moq-rs` documents origin.
