# [S] HOP_ID option

## Goal

A session negotiates the cluster extension with the HOP_ID Setup Option: key
0x40B54, even, so its value is the sender's Hop ID as a bare varint. Neither
implementation recognizes RELAY_HOPS (0x40B55) any more. rs/moq-net and
js/net agree byte for byte, and a peer that sends only the old option is not
negotiated with rather than misread.

## Plan

- `rs/moq-net/src/ietf/cluster.rs`: `RELAY_HOPS` becomes `HOP_ID = 0x40B54`.
  `peer_from_setup` reads it with `get_varint` and `peer_into_setup` writes it
  with `set_varint`, deleting the byte-string round trip and its
  `TrailingBytes` check. `rs/moq-net/src/ietf/parameters.rs` moves the key
  from `ParameterBytes::RelayHops` to `ParameterVarInt::HopId`. The parity
  test asserts even.
- `js/net/src/ietf/parameters.ts` and `cluster.ts`: `SetupOption.RelayHops`
  becomes `HopId = 0x40b54n`, decoded as a varint; the byte fixtures in
  `cluster.test.ts` follow.
- Module docs in both name HOP_ID as the option. No comment records the old
  name.
- Branch from main: the constants and setup codecs are identical on dev, so
  the merge carries it. A wire change, so run `just test smoke-full`.

## Closes

- [#3693](https://github.com/moq-dev/moq/issues/3693) - close this issue when the quest finishes
