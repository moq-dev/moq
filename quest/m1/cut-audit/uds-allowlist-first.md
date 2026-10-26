# [XS] The Unix-socket allowlist is checked before any peer bytes are parsed

## Goal

A local user outside a Unix-socket listener's allowlist is refused on its
peer credentials before the server parses a single byte from it.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Source-inferred,
inherited; local only.

`rs/moq-tokio/src/server.rs:1355` makes the socket `0o666`, and the
allowlist check (`allow.permits`, about line 1516) runs only after
`within(deadline, pending)` has run `qmux::uds::accept`
(`unix.rs:356-372`), so any local user reaches the qmux negotiation parser.
Read `peer_cred` and check the allowlist first, then negotiate. A test
connects as a refused credential and asserts no negotiation runs.

Public API: none. Wire: none.
