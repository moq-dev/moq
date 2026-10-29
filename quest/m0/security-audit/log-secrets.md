# [M] Tokens stay out of logs

## Goal

No credential reaches a log line at any level, on `main` and on `release`:
SETUP and AUTH tokens, `?jwt=` queries in dialed or accepted URLs, and
`Cookie`/`Authorization` headers. Refusal logs escape client-controlled
fields, so a path or root cannot forge a log record.

## Plan

The main-branch fix is in
[moq-dev/moq#7200](https://github.com/moq-dev/moq/pull/7200): redacting
diagnostics on the existing credential types, opaque SETUP parameters,
request-target queries, CONNECT requests, auth requests, and cluster peers;
escaped refusal fields; captured trace tests for SETUP, AUTH, CONNECT, and
refusal. All four tests reproduced leaks before the fix.

Settled under the user's AFK authorization: ✅ retain the existing credential
types with manual redacting `Debug`. This replaces the original shared-type
proposal: moq-net and moq-auth keep their independent public types and byte
fields, avoiding a common crate, a dependency across API layers, and a caller
migration. The redaction stays at each credential-bearing type.

After the main fix merges, backport the leaks present on `release` in a
separate PR against `release`, preserving its published API, per
`CONTRIBUTING.md`. Its SETUP token still derives `Debug`; its protocol and
transport layouts differ from main, so port behavior rather than applying
the patch blindly. This quest stays open until the release backport lands.

Public API: only diagnostic output changes in the current implementation.
Wire: none.
