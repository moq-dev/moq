# [XS] A lapsed request token resets its groups with EXPIRED_AUTH_TOKEN

## Goal

When a moq-transport request token's grant lapses, the group streams of the
subscriptions it admitted reset with the draft's EXPIRED_AUTH_TOKEN code
instead of INTERNAL_ERROR. A client can then tell "renew and retry" from
"denied" without parsing PUBLISH_DONE's reason phrase.

## Plan

Point 1 of [moq-dev/moq#7646](https://github.com/moq-dev/moq/issues/7646)
(ksletmoe-aws), verified on `main`. A lapsed grant ends the subscription with
PUBLISH_DONE status UNAUTHORIZED and reason "token expired"
(`Served::status` in `rs/moq-net/src/ietf/publisher.rs`, which notes that
PUBLISH_DONE has no token-expiry status), and its group streams reset with
INTERNAL_ERROR.

Decided 2026-11-03 (maintainer, `/quest-import`): ✅ reset with
EXPIRED_AUTH_TOKEN; no wire change (the next request's REQUEST_ERROR already
says EXPIRED_AUTH_TOKEN); won't fix. PUBLISH_DONE keeps UNAUTHORIZED, since
its own EXPIRED means the subscription's lifetime ran out.

Check the reset code in drafts 18, 20, and 22. The report cites 0x07 for a
stream reset, while 0x5 is the request error code moq-net already sends
(`rs/moq-net/src/ietf/error.rs`). Use what each draft defines for stream
resets. Test on mocked time that a lapsed grant resets its groups with that
code and a revoked one doesn't, and note it in `doc/bin/relay/auth.md`.

Public API: none. Wire: moq-transport group resets on token lapse change
code.

## Related

- [moq-transport request tokens](/quest/m1/ietf-token-auth.md) - #7646's points 2 to 4 land there
