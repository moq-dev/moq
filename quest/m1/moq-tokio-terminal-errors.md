# [S] moq-tokio's reconnect loop gives up on errors that can never succeed

## Goal

With unlimited backoff, moq-tokio's reconnect loop stops and reports an error
that can never succeed on retry (a local dial or configuration error such as
`UnsupportedScheme`, an invalid URL, or a bad TLS configuration) instead of
retrying it forever. Transient errors keep retrying.

## Plan

Found while planning [moqsrc reconnect](/quest/m1/moqsrc-reconnect.md)
(2026-10-10): `moqsink` already reconnects with `backoff.timeout = 0`, and a
mistyped scheme makes it retry forever without a bus error. `moqsrc` will do
the same once it reconnects.

Classify dial errors at the source: the loop already treats non-retryable
CONNECT statuses (`status_retryable`) as terminal; extend that to local
errors, listed by type rather than by message. Fail loud with the error.
Test with mocked time: an unsupported scheme fails at once under unlimited
backoff, and a refused TCP connect still retries.

Public API: behavior only (the loop ends with the error). Wire: none.
