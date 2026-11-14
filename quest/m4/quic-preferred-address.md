# [M] moq-quic's client migrates to a server's preferred address

## Goal

A moq-quic client that receives a `preferred_address` transport parameter
validates the address once the handshake is confirmed and moves the
connection to it, as RFC 9000 section 9.6 recommends, so a relay that
handshakes on an anycast address and advertises its unicast one
(`--listen-preferred-v4`/`-v6`) gets native clients to follow, not only
browsers.

## Plan

Found by [the QUIC Interop Runner quest](https://github.com/moq-dev/moq/pull/8708):
moq-quic stores the preferred address's connection ID and reset token
(`set_peer_params` in `rs/moq-quic/src/connection/mod.rs`) and never uses
them, while the `listen.rs` docs claimed this crate migrates.

- After handshake confirmation, if the peer sent a preferred address of the
  local socket's family and active migration is not disabled, probe it with
  PATH_CHALLENGE on the preferred address's connection ID (sequence 1), and
  switch the path on PATH_RESPONSE. Keep the original path if validation
  fails, and never migrate a connection the application already closed.
- Cover it on the seeded sim: the switch, a validation that times out, a
  peer that disabled migration, and a preferred address of the other family.
- The QUIC Interop Runner cannot show it yet: of the pinned peers, only
  ngtcp2's server serves `connectionmigration`, and it fails the case
  against every client on <https://interop.seemann.io>. Leave the `known`
  entry in `test/quic-interop/cases.toml` until a pinned peer's server
  passes it with other clients, then drop it.

Not here: serving the runner's server-role `connectionmigration`, which needs
a second socket on one driver endpoint; nothing in MoQ needs that yet.

Public API: none. Wire: none.

## Related

- [Public interop listing](/quest/m4/quic-interop-listing.md) - a red `connectionmigration` column is public there
