# [S] IETF streams that arrive before SETUP are held

## Goal

A moq-transport stream that arrives before SETUP completes is held and
handled once SETUP lands, never aborted and never fatal, in Rust and JS, on
every draft the tree negotiates.

## Plan

Facts (`origin/main`, 2026-09-30):

- Rust server, drafts 17+: `accept_setup` aborts every uni stream that is not
  SETUP (0x2F00) with `UnexpectedStream`, which reaches the wire as
  INTERNAL_ERROR. Bidi streams wait in the transport queue.
- JS: `receiveSetup` reads the first incoming uni stream and fails the whole
  handshake if it is not SETUP.
- Drafts 16 to 21 say early data SHOULD be buffered until the control
  streams arrive, and permit resetting only bidi streams. Drafts 18+ add that
  parameters needing negotiation SHOULD NOT be used before the peer's SETUP.
- Group and fetch decoding needs only the draft version, which ALPN fixes;
  what needs SETUP state is PUBLISH_NAMESPACE (cluster), solicit, and hidden.

Decided (2026-09-30): queue non-SETUP uni streams during the handshake and
hand them to the normal classifier (padding, group, fetch, unknown, per
the uni-stream classifier from moq-dev/moq#4603) once SETUP lands.
QUIC stream credit already bounds the queue, so no extra cap. Do the same in
the JS handshake. Bidi streams keep waiting as today. Check draft 22's text
when it is reachable.

Tests in both languages: a padding stream and a group stream that arrive
before SETUP are handled after it, and the session opens.

Public API: none. Wire: none; fixes conformance.
