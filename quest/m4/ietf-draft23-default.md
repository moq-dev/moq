# [M] Offer moq-transport draft-23 by default

## Goal

`Versions::all()`, `ALPNS`, and the JS default offer include `moq-transport-23`, so a draft-23 peer negotiates it without opt-in, and nothing a draft-23 peer may send closes the session or breaks a request.

## Plan

[moq-dev/moq#8975](https://github.com/moq-dev/moq/pull/8975) landed draft-23 opt-in, in Rust and `js/net`, with these gaps (decided 2026-11-20, maintainer: opt-in first, gaps here):

- REDIRECT is decoded but not acted on. Recommend: treat it as the request's end with an error, the least code, unless a peer shows a use that needs the redirect followed.
- End of Gone Range only decodes; the relay cache has no Gone state. Recommend: no Gone state, since it is moq-transport-only; check that a relay passes the range on as missing data.
- A peer's FILL_PARAMETERS into our relay at draft-23 is tested only moq-net against itself. Run it against a third-party peer, such as imquic's `moq-23` branch.
- MAX_CONTROL_MESSAGE_SIZE is Setup Option 0x09 in the published draft-23 and 0x0A from moq-transport PR 2055 on. We send neither and stay within the 65535 default, which needs no change unless a peer depends on the option.
- Add draft-23 to the `test/interop` version lists and the demo relay configs, and run `just test interop --all` at draft-23.
- Session close codes stay PROTOCOL_VIOLATION (decided 2026-11-20, maintainer).

## Related

- [Fuzz draft-23](/quest/m4/fuzz-ietf-draft23.md) - fuzz coverage of the same codec
