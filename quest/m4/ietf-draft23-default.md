# [M] Offer moq-transport draft-23 by default

## Goal

`Versions::all()`, `ALPNS`, and the JS default offer include `moq-transport-23`, so a draft-23 peer negotiates it without opt-in, and nothing a draft-23 peer may send closes the session or breaks a request.

## Plan

[moq-dev/moq#8975](https://github.com/moq-dev/moq/pull/8975) landed draft-23 opt-in, in Rust and `js/net`. Decided 2026-11-21 (maintainer), one feature at a time:

- REDIRECT: ✅ end the request with an error (rejected: follow it to the new location). It is decoded today but not acted on.
- End of Gone Range: ✅ leave it as is, failing the fetch as unsupported, so the requester gets an error for those groups (rejected: pass the groups on as not found; a Gone cache state).
- A peer's FILL_PARAMETERS (joining subscribe) into our relay: ✅ run it against a third-party peer, such as imquic's `moq-23` branch, and fix what it finds (rejected: rely on moq-net's self-test).
- MAX_CONTROL_MESSAGE_SIZE: ✅ honor a peer's limit (rejected: ignore it). Read the peer's Setup Option and refuse to send a control message past a limit smaller than 65535. It is 0x09 in the published draft-23 and 0x0A from moq-transport PR 2055 on; take the published draft's. We still never advertise one, so what we accept stays at the 65535 default. Mirror it in `js/net`.
- A fill's End of Group: finish the head group at the marker, so a stream that fails after it leaves a complete group instead of an aborted one, and hand a head to the live tail only when no marker ended it. `RunFill` (`rs/moq-net/src/ietf/subscriber.rs`) drops the marker today, and `ServeFill` reads a fill that leaves no head as one that owed it (`Fill::failed`), so this needs a `Fill` state for a head finished at its marker. The range reader (`GroupFetchObjects`) already finishes there. Found by CodeRabbit on [moq-dev/moq#8975](https://github.com/moq-dev/moq/pull/8975).
- Then ✅ offer draft-23 by default (rejected: keep it opt-in; drop the quest): `Versions::all()`, `ALPNS`, the JS default offer, the `test/interop` version lists, and the demo relay configs, and run `just test interop --all` at draft-23.
- Session close codes stay PROTOCOL_VIOLATION (decided 2026-11-20, maintainer).

## Related

- [Fuzz draft-23](/quest/m4/fuzz-ietf-draft23.md) - fuzz coverage of the same codec
