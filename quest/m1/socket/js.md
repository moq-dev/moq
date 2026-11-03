# [S] JS sockets

## Goal

`@moq/net` exports `socket` with the same `Listener`, `dial`, and `Socket`
surface as Rust, and a Rust dialer and a JS listener (and the reverse) open
and accept uni and bi streams through a relay.

## Plan

Follow `doc/concept/socket.md` and mirror the
Rust surface (`rs/moq-net/src/model/socket.rs`), including its track-naming
scheme: streams are unnamed, like QUIC's (`openUni`, `openBi`, `acceptUni`,
`acceptBi`, in JS's casing), and well-known channels are fixed-name tracks on
the socket's `local` and `remote` broadcasts.
Add the Rust/JS pair to `just test interop`, and document the export in
`doc/lib/js/net.md`.
