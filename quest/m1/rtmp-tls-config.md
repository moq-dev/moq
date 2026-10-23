# [S] RTMP server refuses plaintext only alongside TLS

## Goal

`moq_rtmp::Server` cannot be configured to refuse plaintext without TLS.
`with_tls(moq_rtmp::tls::Config { server, plaintext })` replaces
`with_tls(Option<..>)` and `with_plaintext(bool)`, and the runtime check that
makes `accept()` log an error and return `None` forever is deleted.

## Plan

Decided 2026-10-23 (maintainer interview):

- `server` is the `Arc<rustls::ServerConfig>`; `plaintext` says whether
  plaintext RTMP is still served on the same port. Without `with_tls`, the
  server is plaintext only.
- `with_plaintext` is deleted. Breaking for moq-rtmp: add a `MIGRATE.md`
  entry.
- No caller in the repo sets either today; the
  [one-port listener](/quest/m2/one-port/README.md) is RTMPS's planned
  consumer.
