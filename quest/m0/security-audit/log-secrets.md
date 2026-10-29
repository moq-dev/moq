# [M] Tokens stay out of logs

## Goal

No credential reaches a log line at any level, on `main` and on `release`:
SETUP and AUTH tokens, `?jwt=` queries in dialed or accepted URLs, and
`Cookie`/`Authorization` headers. Refusal logs escape client-controlled
fields, so a path or root cannot forge a log record.

## Plan

From the 2026-10-27 audit (AUTH-3, AUTH-4, RELAY-8), verified at
[8d2fa6653a](https://github.com/moq-dev/moq/commit/8d2fa6653a). The audit
called these latent; they are reached today:

- Every decoded message is logged with `trace!(?msg)`
  (`rs/moq-net/src/lite/message.rs`, `ietf/message.rs`), and `lite::Auth`,
  `ietf::Auth`, and `moq_auth::Token` derive `Debug` over the raw token.
- `rs/moq-tokio/src/quic/web/connect.rs` logs the whole WebTransport CONNECT
  request at **debug**, including every lite-06-and-older browser client's
  `?jwt=` and all headers.
- `rs/moq-relay/src/config.rs` traces the resolved config, and `Peer`'s
  `Debug` (`cluster.rs`) redacts `token` but prints `url`, where
  `--cluster-connect https://host/?jwt=` keeps it.
- `rs/moq-auth/src/serve.rs` renders the refusal's `path` (client-chosen) and
  verified `root` with `%`, unescaped.

Fix at the type, not per call site: one redacting token type with a manual
`Debug` shared by both protocols and moq-auth, and URL `Debug`/log output that
strips the `jwt` query (the HLS and js/net redaction commits
[ce17f7e76a](https://github.com/moq-dev/moq/commit/ce17f7e76a) and
[3a82ece225](https://github.com/moq-dev/moq/commit/3a82ece225) are prior
art). Log refusal fields with `?`. A test asserts a known token never appears
in captured output at trace level for a full SETUP, CONNECT, and refusal.

Backport: `release` lays these out differently (moq-lite, web-transport
crates); check each leak there and backport the ones present in a separate PR
against `release` per `CONTRIBUTING.md`.

Public API: `Debug` output changes only; if the shared token type is exported,
report it. Wire: none.
