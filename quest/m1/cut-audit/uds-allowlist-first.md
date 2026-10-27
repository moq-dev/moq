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

### Checkpoint (2026-10-26)

The source fix checks the existing credential allowlist inside the private Unix
pending handshake, before `qmux::uds::accept` can read or write. Public API,
wire format, allowed negotiation, and allowlist matching semantics are unchanged.

A real local socket test reproduced the original boundary: a refused UID received
QMux's first preface byte (`64`). The fixed test closes that silent peer without
any negotiation bytes and also completes ALPN negotiation for an allowed UID.
The four Unix server tests pass, including stalled-peer concurrency, shutdown,
and the configured publisher. No host user, timer, or credential was changed.

Validation remains unfinished. `nix develop --command just fix` fails in the
unchanged `rs/moq-mux/benches/catalog.rs:226`: `hang::catalog::MAX_RENDITIONS`
does not exist. The two changed source files are formatted separately. Finish
the existing benchmark repair, then run the full required `just fix` and clean
committed-head `just ci` before ready/review. Do not bypass a host limitation
reported by that gate. There is no API, wire, or allowlist-policy decision.
