# [M] A reconnecting client is established by the credential's answer

## Goal

A reconnecting client (Rust `Reconnect` and `moq_tokio::Connection`, JS
`Reload`, and every binding through `moq-ffi`) counts a session as established
once the peer accepts the connection credential, instead of after it stays up
`Backoff::healthy` (1 s). A close before that answer is a refusal and ends the
policy, as today. Versions with no acceptance message keep the uptime bar. No
wire change.

The tests [moq-dev/moq#8817](https://github.com/moq-dev/moq/pull/8817) gave
real 1 s waits (Go, Python, moq-c), and the OBS tests in draft
[moq-dev/moq#9016](https://github.com/moq-dev/moq/pull/9016), drop the session
right after it is accepted instead.

## Plan

Found by the triage of Nightly run
[44127032469](https://github.com/moq-dev/moq/actions/runs/44127032469)
(2026-11-17): the OBS output and source tests drop a session the moment it goes
live, which #8817 now treats as a refusal. #9016 fixes them with real 1 s
waits; the maintainer chose this quest instead, and #9016 waits on it.

Where acceptance already shows (researched 2026-11-17):

- lite-05/06 and moq-transport: the server sends SETUP only from `ok()`, and
  `moq-relay` judges the token before `ok()`
  (`rs/moq-relay/src/connection.rs`), so a refused session never sends it.
- lite-07: also the AUTH_OK answering each side's empty-token Auth Stream,
  which carries the grant for the connection credential.
- lite-01/02: the SERVER_SETUP reply.
- lite-03/04: nothing. They keep `Backoff::healthy` as the bar.
- `Tokens::credential` (`rs/moq-net/src/auth/tokens.rs`) already computes this
  verdict across versions; `js/net/src/connection/tokens.ts`'s `Verdict` is its
  counterpart. Reuse them rather than reading SETUP again.

Decided 2026-11-17 (maintainer, /quest-plan):

- Signal: ✅ the existing credential verdict (server SETUP, or AUTH_OK on
  lite-07) (rejected: a new SETUP_OK message, which duplicates AUTH_OK; 1 s of
  uptime for every version).
- ✅ Versions with no acceptance message keep a timeout, `Backoff::healthy`.
- Update `Handover.tla` and the verify-reconnect skill where `established` is
  set, and the `Backoff` docs in every binding.

## Related

- [Connect policy state](/quest/m4/connect-policy-state.md) - moves the same policy into one place per language
