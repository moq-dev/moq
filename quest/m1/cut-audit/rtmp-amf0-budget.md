# [S] AMF0 decoding costs no more than the bytes a peer sent

## Goal

An RTMP peer cannot make a listener allocate more memory for decoded AMF0
values than a small constant factor of the message bytes it already paid
for under the connection's assembly budget. One 16 MiB message of Null bytes
no longer allocates about 940 MB before connect or auth.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Source-inferred and
spot-verified, inherited. Reachable on any host running
`moq import rtmp --listen`.

`amf0::deserialize` (`rs/moq-rtmp/src/rml/amf0/deserialization.rs:18`)
pushes one `Amf0Value` (56 bytes) per input value, so a type-18 data message
of 16,777,215 `0x05` bytes (accepted under `MAX_ASSEMBLY_BYTES`, 64 MiB)
becomes 16.7M values, then is discarded. It is reached from
`rml/messages/message_payload.rs:57` for message types 15, 17, 18, and 20
via `ServerSession::handle_input`. Object properties amplify about 25x.

Fix direction, bounded by design rather than a value-count constant:
decode commands lazily into the typed fields the session actually reads
(command name, transaction id, the few `connect`/`publish`/`play` fields),
and skip the rest by length, or charge each decoded node against the
connection's assembly budget. A test feeds the Null-byte message and asserts
the decode's peak allocation stays within a small multiple of the input.

Public API: none. Wire: none.

## Related

- [AMF0 exact reads](/quest/m1/rtmp-amf0-exact-reads.md) - the truncated-string fix in the same decoder
- [RTMP sessions follow one strict path](/quest/m1/cut-audit/rtmp-session-strict.md) - the session layer above this decoder
