# [M] Malformed moq-lite input closes the session

## Goal

On moq-lite, a peer that breaks a control-stream rule gets its session
closed with PROTOCOL_VIOLATION, in both `rs/moq-net` and `js/net`, and a
Report-level publisher answers PROBE targets instead of resetting the stream.
The rules are unchanged from lite-06 through lite-07-wip, so the fixes apply
to every lite version. Data-stream (FRAME) decode errors stay per-stream, as
the draft says, and a reset or FIN is never a violation.

## Plan

Found by an external interop runner and verified against the code 2026-10-12.
Rust-to-Rust and Rust-to-JS sessions never hit these; third-party peers do.

- SETUP (#5257). A client that receives Path or Role in the server's SETUP
  closes with PROTOCOL_VIOLATION. `lite/setup.rs` decodes Role through
  `Role::from_code`, which turns 0 and unknown values into `None`, so keep
  the parameter's presence; a server still reads an unknown Role as Both.
  Check at the stored peer SETUP (`lite/subscriber.rs` `UniState::Setup`,
  `js/net/src/lite/connection.ts`), which already closes on a
  ProtocolViolation. Decided 2026-10-12: also make the server close on Path
  over a binding whose handshake carries a request URI (WebTransport,
  WebSocket), since the draft sentence is the same.
- PROBE (#5258). `ProbeServe::poll_probe` in `lite/publisher.rs` polls
  `reader.poll_closed`, which fails on any pending byte, so the first target
  resets the stream with MALFORMED_TRACK. Decode targets with `lite::Probe`
  instead; at Report level ignore the value and keep reporting, and only FIN
  or a reset ends the stream. In JS, `runProbe` never reads the stream, so a
  buffered target probably hides the subscriber's FIN; read targets there
  too. Decided 2026-10-12: our subscribers keep sending no target. Unit-test
  the publishers; the Increase-level response belongs to
  [Discover capacity](/quest/m1/quic-probe.md).
- Control decode errors (#5259). Decided 2026-10-12: any decode error on a
  control stream closes the session with PROTOCOL_VIOLATION, not just the two
  cases cited (GOAWAY URI over 8,192 bytes, Message Length mismatch).
  `Control::poll` closes on `Error::Decode(_)` instead of logging. Delete the
  comment that a malformed GOAWAY must not tear down a draining session: the
  draft says MUST. Do the same in JS `#runBidis`, which closes today only on
  `ProtocolViolation`. Also enforce that a server receiving a non-empty
  GOAWAY URI closes the session. Land the PROBE fix first or in the same PR,
  or a third-party PROBE target would close the whole session.
- The rule reaches every control decoder, not only `Control::poll` and
  `#runBidis`. Child handlers that swallow a peer's decode error must
  propagate it: `RequestServe::poll` resets and returns `Ok(())` for a
  malformed SUBSCRIBE_UPDATE decoded after `RequestState::Decode`, and
  `ProbeServe::poll` consumes `poll_probe`'s error. Response decoders on
  streams we opened count too: Rust `RecvBandwidth::poll` logs a malformed
  PROBE report from `ProbeStream::poll`, and JS `Subscriber.runProbe` catches
  it. Prefer one shared close path over per-handler fixes.
- One test per rule per language, driving raw bytes like the runner does,
  including a malformed SUBSCRIBE_UPDATE and a malformed PROBE report.
  Share the session-close path that malformed AUTH messages already use.

Public API: none. Wire: none; the behavior now matches the draft.

## Closes

- [#5257](https://github.com/moq-dev/moq/issues/5257) - client accepts Path or Role in the server's SETUP
- [#5258](https://github.com/moq-dev/moq/issues/5258) - Report-level publisher resets the Probe Stream on a target
- [#5259](https://github.com/moq-dev/moq/issues/5259) - control decode errors end only the stream

## Related

- [Malformed moq-transport input](/quest/m2/ietf-malformed-close.md) - the same rule for draft-18 and draft-21
