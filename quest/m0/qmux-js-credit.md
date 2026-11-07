# [S] JS qmux keeps credit and final sizes consistent with Rust

## Goal

A JS qmux session (the WebSocket fallback) never stalls or gets closed by a Rust peer because of its own flow-control bookkeeping, and a peer cannot grow its memory without bound:

- Bytes that arrive on a stream we already stopped (late STREAM frames, and the final-size gap of a later RESET_STREAM) count toward connection receive credit. Today `js/qmux/src/session.ts` (`#onStream`, `#onResetStream`) returns early once the receive half is deleted on cancel, so each cancelled stream permanently shrinks the window until MAX_DATA is never sent again.
- A RESET_STREAM's final size is at least every byte already handed to the socket. Today `sendOffset` advances only after `scheduler.enqueueStream` resolves, so a STOP_SENDING handled while a frame is mid-`sink.write` replies with a short final size, which Rust's `rs/moq-quic/src/connection/streams/recv.rs` refuses with FINAL_SIZE_ERROR.
- MAX_DATA, MAX_STREAMS_{BIDI,UNI} and MAX_STREAM_DATA (per stream) replace any queued copy, as Rust does with its pending flags, instead of each taking a new entry on the uncapped control lane.
- An empty binary WebSocket message is a no-op on every qmux version, matching `rs/qmux/src/transport/websocket.rs`; qmux-00 currently throws.
- WebSocket messages larger than the record size are refused before they are buffered where the platform allows it: Rust `rs/qmux/src/ws.rs` `Upgraded` validates or sets the tungstenite `max_message_size` itself instead of trusting the caller, and JS documents the `ws` `maxPayload` it needs.

## Plan

Found by the 2026-11-07 invariant audit (qmux-1 to qmux-5). qmux-1 and qmux-2 are the urgent ones: an honest Rust relay triggers both whenever a JS app cancels group streams over WebSocket.

- Keep each stopped peer stream's receive offset (and final size once known) until FIN or RESET so late bytes are accounted, and port Rust's `credit_returns_for_unread_and_stopped_streams` (`rs/moq-quic/src/connection/mux/tests.rs`).
- Count bytes as sent when the scheduler takes the frame from its queue, or track the in-flight frame per stream and include it in the RESET final size. Test with a sink whose `write` is held open while STOP_SENDING arrives.
- Send the MAX_* frames through `replaceControl` keyed by type (and stream id for MAX_STREAM_DATA); fix the "bounded" claim in `scheduler.ts`.
