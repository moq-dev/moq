# [M] Raw QUIC stream codes stay raw

## Goal

On a raw QUIC session (`moqt://`, `moql://`, raw iroh), RESET_STREAM and
STOP_SENDING carry the application's code as-is, not mapped through the
HTTP/3 WebTransport code space, so moq agrees with other raw QUIC MoQ stacks
on stream errors. WebTransport sessions keep the mapping they need.

## Plan

- `web-transport-moq` (moq-dev/noq) runs every stream code through
  `web_transport_proto::error_to_http3` and `error_from_http3` in `send.rs`
  and `recv.rs`, whether or not the session came from `Session::raw`.
  `web-transport-iroh` and `web-transport-quinn` (moq-dev/web-transport) do
  the same. A raw peer's code 5 reads as `None` or another value, and ours
  reaches it as a large HTTP/3 code.
- [Close codes](/quest/m1/close-codes.md) fixed the same mix-up for
  `ApplicationClosed` (noq#11, #4262). Let each stream know whether its
  session is raw and skip the mapping there, in all three adapters, with a
  round-trip test per adapter against a plain QUIC peer.
- Release the fixed crates and bump the pins here in the same quest; published
  crates depend on crates.io releases, never a patch. A moq-tokio test over
  `moqt://` asserts a reset code arrives verbatim, beside `close_code.rs`.
- Mixed versions: two moq peers on raw QUIC agree today because both map. A
  fixed peer and an older one will not. Check which codes moq-net reads back
  (group stream resets, subscribe STOP_SENDING) and decide whether the
  transition needs more than a release note before merging.

Public API: none expected. Wire: raw QUIC stream error codes become the
application's own values, a behaviour change to call out in the PR.

## Required

- [Close codes](/quest/m1/close-codes.md) - the adapter releases and trait 0.5 this builds on
