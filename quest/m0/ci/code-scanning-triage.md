# [XS] Code scanning has no untriaged alert on main

## Goal

Every open code-scanning alert on `main`'s default branch is either fixed
or dismissed with a reason and a comment that names the evidence:
the `rust/cleartext-logging` alerts
[58](https://github.com/moq-dev/moq/security/code-scanning/58),
[59](https://github.com/moq-dev/moq/security/code-scanning/59), and
[61](https://github.com/moq-dev/moq/security/code-scanning/61), and the
`rust/hard-coded-cryptographic-value` alerts
[35](https://github.com/moq-dev/moq/security/code-scanning/35),
[36](https://github.com/moq-dev/moq/security/code-scanning/36), and
[64](https://github.com/moq-dev/moq/security/code-scanning/64).

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7362](https://github.com/moq-dev/moq/pull/7362) and [moq-dev/moq#7027](https://github.com/moq-dev/moq/pull/7027). CodeQL: ✅ Dismiss + triage quest. Milestone: ✅ m0 (user: "use recommended for any CI/flakes").

Alerts [67](https://github.com/moq-dev/moq/security/code-scanning/67) and
[68](https://github.com/moq-dev/moq/security/code-scanning/68) were
dismissed on 2026-10-31 as "used in tests" (test assertion messages in
`rs/moq-relay/src/web.rs`'s `mod tests`); follow that precedent, and the
earlier dismissals 25 to 34. Open alerts, as of 2026-10-31 on `38566f5a84`:

- **58** `rs/moq-auth/src/client.rs:1150` and **61**
  `rs/moq-rtc/src/lib.rs:434`: both are inside `mod tests` (from
  `client.rs:497` and `lib.rs:74`). The "secret" is the literal strings
  `jwt`, `secret`, and `user:pass` in an assertion that proves those never
  appear in errors or logs. Dismiss as "used in tests".
- **59** `rs/moq-tokio/src/server.rs:1416`: production code. CodeQL traces
  `read_certs(...)` / `certs.load_certs(...)` (`rs/moq-tokio/src/tls.rs:211`,
  `:1364`, `:1546`) into a log. Certificates are public, but confirm the
  flow cannot carry the private key or its path's contents (for example
  through a TLS load error's message); fix the log if it can, otherwise
  dismiss as a false positive with that reasoning. #7027 already judged
  the `server.rs` cert alert a false positive.
- **35**, **36** `rs/moq-tokio/src/mdns.rs:778-779`: the test keys
  `KEY_A`/`KEY_B` inside `mod tests` (from `:775`). Dismiss as "used in
  tests".
- **64** `rs/moq-quic/src/varint.rs:142`: the zeroed scratch buffer a
  varint decodes into, not a key. #7027 judged it a false positive; dismiss
  as one.

Dismiss through `gh api -X PATCH repos/moq-dev/moq/code-scanning/alerts/<n>`
with `state=dismissed`, the reason, and a comment ending in the model line.
If a fix is needed, it lands in this quest's PR; a dismissal-only finish
needs no code change, so the PR only deletes this quest.

Public API: none. Wire: none.
