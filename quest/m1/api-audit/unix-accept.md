# [S] Unix sockets accept only through the server's allowlist

## Goal

No public moq-tokio API runs the qmux handshake on a Unix socket before the
peer allowlist is checked. Embedders accept Unix peers through the server's
`unix` config, which checks `allow` against the peer credentials first.

## Plan

Found by [moq-dev/moq#6907](https://github.com/moq-dev/moq/pull/6907): the
public `unix::Listener::accept` (`rs/moq-tokio/src/unix.rs`) calls
`accept_pending(None)`, so it parses qmux from every local user and returns
`PeerCred` for the caller to check afterwards. The server path
(`spawn_unix_loop` in `server.rs`) passes `config.unix.allow` and refuses
before the handshake.

Decided 2026-10-27 (maintainer): make the accept path crate-private rather
than move the allowlist onto the Listener. Nothing outside moq-tokio calls it,
so this deletes API with no consumer. Keep public only what the server config
needs, and log a refused peer once (it is logged twice today).

Public API: breaking for moq-tokio; `unix::Listener::accept` (and anything
public only to serve it) goes. Wire: none.
