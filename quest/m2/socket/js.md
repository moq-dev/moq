# [S] JS sockets

## Goal

`@moq/net` exports `socket` with the same `Listener`, `dial`, and `Socket`
surface as Rust, and a Rust dialer and a JS listener (and the reverse) open
and accept uni and bi streams through a relay.

## Plan

Follow `doc/concept/socket.md` and mirror the
[Rust surface](/quest/m2/socket/rust.md), including its track-naming scheme.
Add the Rust/JS pair to `just test interop`, and document the export in
`doc/lib/js/net.md`.

## Required

- [Rust sockets](/quest/m2/socket/rust.md) - the reference surface and concept page
