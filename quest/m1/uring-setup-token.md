# [S] The ring forwards the SETUP token to auth

## Goal

Under `--runtime-io-uring`, a moq-transport client's SETUP `AUTHORIZATION
TOKEN` reaches the auth decider byte for byte, over raw QUIC and
WebTransport, as it already does on the tokio path. Today the ring drops it:
a client whose only credential is that token is admitted anonymously or
refused, and public rules admit it with the public grant instead of refusing
it (`presents_token` in `rs/moq-relay/src/auth.rs`).

Out of scope: folding the ring onto `Connection`. The ring admits on the
shared runtime and drives the session on its worker, and that split is what
keeps it a separate listener.

## Plan

Confirmed on `main` (`623b5291c`): `serve_request` in
`rs/moq-relay/src/uring.rs:727` builds its `moq_auth::Request` at lines
744-776 from the SETUP path, the URL, the link facts, and the role, and never
reads `Handshake::token()` (`rs/moq-net/src/server.rs:855`). The tokio path
copies it in `request_for` (`rs/moq-relay/src/auth.rs:1034`) from
`moq_tokio::server::Request::token()`. Found in #5917's review.

Decided by the maintainer on #5957:

- ✅ Milestone m1, not m2: it is an auth bug, so it lands on the
  breaking-release horizon even though the ring is opt-in and not yet
  packaged. It also stays in the packaged-relay parity bar.
- ✅ One helper builds the request from a moq-net SETUP: path and `?query`
  split from the SETUP path (`/` when empty), the token, and the role, given
  the auth transport. It sits in `auth.rs` beside `request_for`. The ring
  calls it, then replaces the path and query with the URL's on WebTransport
  and adds the link facts (remote, local, SNI, ALPN, TLS identity). The
  token comes from the SETUP on both transports, since a moq-transport
  client over WebTransport still sends it there.
  Not chosen: the ring building #5917's `Accepted` and calling its
  `admission()` (that name clashes with the ring's private `Accepted`, and
  it ties this fix to #5917); folding the ring onto `Connection` (see Goal).
- ✅ Ordering with #5917: not blocked on it. #5917's `Accepted::admission`
  copies the same SETUP facts, so whichever lands second makes it call the
  helper, leaving one copy.
- ✅ Regression test `uring_forwards_the_setup_token` in
  `rs/moq-relay/tests/integration/runtime_uring.rs`. An embedded decider
  (`relay.admissions()`) asserts `request.token` equals the sent token and
  grants. The client hand-encodes a draft-16 CLIENT_SETUP carrying the token,
  as `forwards_the_setup_token` in `tests/integration/auth_lifetime.rs`
  does, once over raw QUIC (ALPN `moqt-16`) and once over WebTransport (the
  `moqt-16` subprotocol, with the URL's path and query asserted too). It
  fails on `main` because the token is `None`. It runs in `just rs uring`,
  which `uring.yml` runs on affected PRs and nightly runs on `main`.

Public API: none (the helper is crate-private). Wire: none.

## Related

- [In-band auth](/quest/m1/auth/README.md) - the SETUP token is that questline's connection credential on moq-transport
