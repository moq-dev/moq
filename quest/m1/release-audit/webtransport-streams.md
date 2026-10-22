# [M] Find HTTP/3 control streams independently of arrival order

## Goal

Tokio WebTransport connects to legitimate peers whose QPACK or other
unidirectional streams arrive before SETTINGS. A partially received stream
header must not block classification of another ready control stream.

## Plan

P2 interoperability issue. At main 6369eca,
`rs/moq-tokio/src/quic/web/settings.rs:47-49` passes the first `accept_uni()`
result directly to the SETTINGS reader, which requires stream type zero.
Both client and production server use this path. A loopback reproduction
using current moq-tokio and trusted local TLS opened a QPACK encoder stream,
wrote `[0x02]`, then opened a control stream with WebTransport SETTINGS.
It failed with `SettingsError(ProtoError(UnexpectedStreamType(StreamUni(2))))`.
This is an observed connection failure, not merely a spec-conformance concern.

The code was imported in
[moq-dev/moq#5664](https://github.com/moq-dev/moq/pull/5664). Its external
release dependency may share the bug; main-only regression status is unproven.

Classify pending HTTP/3 streams independently, retain required critical
streams, and bound ownership through transport credit. Reuse the established
uring behavior where suitable (`rs/moq-uring/src/quic/web.rs:180-248`). Test
QPACK-first, reordered control arrivals, partial headers, unknown types, and
shutdown. Do not add a retry or timeout workaround. No API or wire-format
change is expected. Verify release dependency behavior and propose a backport
if affected.

## Related

- [Receive memory](/quest/m1/quic/receive-memory.md) - classification must preserve connection-credit ownership
- [QPACK dependency fix](/quest/m0/web-transport-proto-0.6.3.md) - separate decoded-header expansion problem
