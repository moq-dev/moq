# [XS] Update draft-lcurley-qmux-websocket for qmux-VV ALPN tokens

## Goal

`drafts/draft-lcurley-qmux-websocket.md` specifies the `qmux-VV.<alpn>` and
bare `qmux-VV` WebSocket subprotocol tokens that `rs/qmux`, `rs/moq-tokio`,
and `js/qmux` negotiate on the wire, and `just drafts check` passes.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ update `draft-lcurley-qmux-websocket.md` before the
`0.22.0` cut alongside deleting the lite-07 `raw_parts` migration (rejected:
fixing only lite-07; leaving the draft out of sync).

- `rs/qmux/src/alpn.rs:32-65`, `rs/qmux/src/ws.rs:258,446-477`, and
  `rs/moq-tokio/src/websocket.rs:525,793-814` negotiate `qmux-VV.<alpn>` (and
  bare `qmux-VV` for default/unspecified ALPN) in `Sec-WebSocket-Protocol`.
- `drafts/draft-lcurley-qmux-websocket.md:36,76-86,120-124,162-167` still
  describes negotiating bare QUIC ALPN tokens directly.
- Rewrite the negotiation section, examples, and IANA/token grammar in
  `drafts/draft-lcurley-qmux-websocket.md` to match the shipping `qmux-VV`
  token format without conflating it with the post-cut
  [qmux-02 subprotocols](/quest/m4/qmux-02-subprotocols.md) work.

Public API: none. Wire: none (draft updated to match existing wire). Verify
with `just drafts check`.
