# [S] Rust tokens carry the connection credential in the URL auth parameter

## Goal

`moq-net`'s `Tokens` and `moq-tokio`'s `Connection` carry the connection credential, the first token, in the URL's `?auth=` instead of SETUP and `?jwt=`, as `js/net`'s `urlAuth: true` does, with a fresh credential on each dial and `answered()` judging the peer's verdict on it.

## Plan

Decided 2026-11-20 (maintainer, [comment](https://github.com/moq-dev/moq/pull/9215#issuecomment-7053898194) on [moq-dev/moq#9215](https://github.com/moq-dev/moq/pull/9215)): keep the Rust counterpart as a quest rather than drop it. Placed in m4 by m3's rule: Kixel's node, the one caller, dials with the query itself today, so no one waits on it.

Mirror the JS behavior in `rs/moq-net/src/auth/tokens.rs` (`Tokens::preview`, `locate`) and the connection that builds each dial in `rs/moq-tokio/src/connection.rs`:

- The first token is replaced on each dial in the URL's `auth` parameter, an existing `auth` is replaced, and the configured URL is untouched, so the retry sequence and backoff do not restart.
- A URL that already has a `jwt` keeps it, and every token then rides AUTH.
- A replacement of the first token on a live session waits for the next dial; it rides no AUTH stream.
- The WebSocket fallback presents the same carrier.

Name the option `url_auth` on the Rust surface that takes the tokens, mirroring JS, as a typed field of the existing struct rather than a `with_*` twin. Test the dial URL it builds against the credential parser, and a replacement refused on the redial that `answered()` rejects while the loop stays live.

Public API (Rust, not breaking): one new option. Wire: none.
