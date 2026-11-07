# [M] JS qmux streams follow QUIC's per-stream state machine

## Goal

`js/qmux` tracks each stream's send and receive halves through the states of [RFC 9000 §3](https://www.rfc-editor.org/rfc/rfc9000#section-3), as `rs/moq-quic/src/connection/streams/state.rs` does, instead of credit bookkeeping spread across `session.ts` handlers. A JS qmux session (the WebSocket fallback) then cannot leak or double-count credit by construction:

- A stream's flow record lives until both halves are terminal on the wire (all data or a reset received and consumed; FIN or RESET sent), not until the app releases its handle. Today `#maybeDeleteStreamFlow` drops it on cancel, so late STREAM bytes and a later RESET's final-size gap are never counted and the connection window shrinks until MAX_DATA stops (an honest Rust peer stalls the session).
- Connection and stream credit change only inside state transitions, so no handler can return early past the accounting (today twelve call sites do it by hand).
- The send half's offset counts bytes when the scheduler takes them, so a RESET_STREAM's final size covers a frame mid-`sink.write`. Today it does not, and Rust closes the connection with FINAL_SIZE_ERROR.
- Window updates (MAX_DATA, MAX_STREAMS_{BIDI,UNI}, MAX_STREAM_DATA) are pending flags or per-stream latest values drained by the scheduler, as Rust's `pending` flags are, not entries on the uncapped control lane.
- An empty binary WebSocket message is a no-op on every version, and messages over the record size are refused before buffering where the platform allows it (Rust `rs/qmux/src/ws.rs` `Upgraded` validates its own `max_message_size`; JS documents `ws` `maxPayload`).

## Plan

Found by the 2026-11-07 invariant audit (qmux-1 to qmux-5); qmux-1 and qmux-2 are reproduced as tests before the redesign.

Decided 2026-11-07 (maintainer, invariant-audit root causes): fix the cause, the hand-rolled flow accounting, rather than patching each early return. Mirror Rust's state shapes and port its credit tests (`credit_returns_for_unread_and_stopped_streams` in `rs/moq-quic/src/connection/mux/tests.rs` among them) so both implementations pin the same invariants. A single implementation for both languages is the longer-term route, planned in [Evaluate WASM-to-JS for @moq/net](/quest/m5/rs2ts-wasm2js.md); this does not wait for it.
