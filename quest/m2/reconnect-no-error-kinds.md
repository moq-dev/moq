# [S] The reconnect loop stops judging errors by kind

## Goal

After a client's first session, the reconnect loop retries every failure
with backoff, whatever the error, a session-level protocol violation or a
refused credential included. `Verdict::from(&Error)` and the `Refused` and
`Fatal` verdicts that sort session errors by kind are gone from Rust, and so
is the classification built on them in moq-tokio: `verdict()` in
`rs/moq-tokio/src/connection.rs`, which maps auth errors and HTTP statuses
onto `Refused`, `Fatal`, and `Settled`, and `Error::is_terminal` in
`rs/moq-tokio/src/error.rs`, which reads `Verdict::from`. The JS counterpart
in `js/net/src/connection/reload.ts` (an auth rejection that stops the URL)
goes too.
Verdicts that are the address walk's own policy rather than an error kind,
`Unusable` for an unsupported scheme and `Settled` for an answer that holds
for every address, stay.

## Plan

Decided 2026-11-10 (maintainer, `/quest-import` of
[moq-dev/moq#8451](https://github.com/moq-dev/moq/issues/8451)):
✅ delete the error-kind matching (rejected: keep it). AGENTS.md says never
match on errors, and with first-dial fail-fast (`Reconnect::established`)
a setup mistake already ends the client before any session.

If what remains of `Verdict` no longer distinguishes anything the walk
acts on, delete the type rather than keep a one-variant enum.

Accepted cost: a credential that expires during a long run retries slowly,
logging the refusal each time, until the tokens refresh. The token-change
check that today lets a refused dial try again goes with it, since every
failure now retries.

m2 because `Verdict` and `Reconnect::failed(.., verdict)` are public, so
removing variants breaks callers; the PR's Impact section gives the
migration.

Run the `verify-reconnect` skill's TLA+ model of the connect policy, and
update the model with the new rule.

Public API: breaking (`Verdict` loses `Refused` and `Fatal`, and
`From<&Error> for Verdict` goes). Wire: none.
