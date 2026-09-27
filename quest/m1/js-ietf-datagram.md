# [M] JavaScript moq-transport datagrams

## Goal

`@moq/net` sends and receives datagrams over moq-transport as
`OBJECT_DATAGRAM`, matching Rust: one Object at ID 0 is a single-frame group
whose Group ID is the sequence. A JavaScript publisher's datagrams reach a Rust
subscriber, and a Rust publisher's reach a JavaScript one.

## Plan

Port `rs/moq-net/src/ietf/datagram.rs` and the session's send and receive
loops. Decode every draft's Type flags, drop what the model cannot carry the
same way Rust does, and close the session on a malformed Type.

The integration test `ietf does not deliver datagrams` flips to delivery on
every supported draft, and `just test interop --all` covers both directions.

## Related

- [Datagram range](/quest/m1/datagram-range.md) - the subscribe range for datagrams, settled on both protocols
