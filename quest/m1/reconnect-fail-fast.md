# [M] A client's first dial fails fast, and both sides log a version mismatch

## Goal

A client whose first walk of the peer's addresses establishes no MoQ session
stops and returns that walk's error instead of retrying. Once a session has
been established, the reconnect loop retries later failures with backoff, as
it does today: each failed handshake grows the delay, and a session that stays up
for `Backoff::healthy` (1 s by default) resets it, so relay restarts and
redeploys still recover. When the client and relay share no MoQ version, the
relay logs the versions offered against those it accepts, and the client
logs the refusal. Rust (`rs/moq-net/src/reconnect.rs`), JS
(`js/net/src/connection/reload.ts`), and moq-ffi's reconnect behave the same.

## Plan

Decided 2026-11-10 (maintainer, `/quest-import` of
[moq-dev/moq#8451](https://github.com/moq-dev/moq/issues/8451)).
In the maintainer's words: "any failure during the handshake increases the
backoff; completing the handshake (plus 1s) resets the backoff; the first
connection attempt never retries, so we fail fast."
✅ that rule (rejected: typed errors for version mismatch and untrusted
certificates; a moq2ts-style cap of N failed attempts before a first
session; retrying only when the previous attempt completed a handshake,
which ends every client whose first retry lands while its relay restarts).

Why: the maintainer doesn't want retry decisions to depend on error kinds,
which miss cases as errors change. Failing the first dial ends the reported
loop (no common version), an untrusted certificate, and every other setup
mistake, with no error classification.

Interpretation, recorded so it isn't asked again: "the first connection
attempt" is the first walk, so address fallback within that walk still runs.
A client started before its relay is reachable, such as a compose stack or
a phone launching offline, now exits, and its supervisor or app restarts it.
Say so in the reconnect docs and in moq-ffi's session docs.

Version-mismatch logs: the relay's only line today is `failed to read
capsule e=UnexpectedEnd` from `moq_tokio::quic::web::session`, and the
client logs only `connecting` and `reconnecting after backoff`. Reproduce
with the issue's commands (`moq-relay --listen-version moq-lite-07-wip`
against a client on its default versions) before fixing.

The error-kind matching in `Verdict::from(&Error)` is deleted by
[the follow-up](/quest/m2/reconnect-no-error-kinds.md), since `Verdict` is
public and its removal breaks. This quest leaves `Verdict` as it is, so after
a first session a `Refused` or `Fatal` verdict still ends the loop until
that quest lands.

Run the `verify-reconnect` skill's TLA+ model of the connect policy, and
update the model with the new rule.

Public API: no signature changes; `Reconnect` and `Reload` change behavior
on the first walk (name it in the PR's Impact section). Wire: none.

## Closes

- [#8451](https://github.com/moq-dev/moq/issues/8451) - with no MoQ version in common, a client retries forever and neither side logs why

## Related

- [moq-relay restarts without dropping sessions](/quest/m3/relay-restart.md) - the restarts this retry rule must keep surviving
