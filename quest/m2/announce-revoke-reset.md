# [S] A revoked announce resets instead of FINning

## Goal

A moq-transport PUBLISH_NAMESPACE we accepted whose token is revoked after
its OK is withdrawn by resetting the request, RESET_STREAM and STOP_SENDING
with INTERNAL_ERROR, as draft-22 §6.4.2.3 withdraws a request, instead of
ending with a FIN.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- m1
- ✅ m2

From [moq-dev/moq#7703](https://github.com/moq-dev/moq/pull/7703)'s
follow-ups. That PR resets a lapsed announce with EXPIRED_AUTH_TOKEN (0x7)
but leaves a revoked or narrowed one ending with a FIN, as before.

- Use INTERNAL_ERROR, since the stream registry has no "unauthorized" code.
- This changes behavior for revocations: a peer that read the FIN as the end
  now sees a reset, and the reset takes an unflushed OK with it, as #7703's
  expiry reset does. Note it in moq-net's MIGRATE.
- Reuse #7703's `Reader::stop` and `Writer::reset`.
- Rewrite #7703's FIN-and-wait revoke test
  (`a_revoked_token_retracts_an_announce_whose_ok_is_blocked`) to expect the
  reset, on mocked time for drafts 18, 20, and 22.

Public API: none. Wire: existing codes only; a revoked announce now resets.

## Required

- [A lapsed request token resets its groups with EXPIRED_AUTH_TOKEN](/quest/m1/auth-expiry-reset.md) - adds the announce reset path this reuses (moq-dev/moq#7703)
