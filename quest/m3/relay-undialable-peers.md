# [S] The relay refuses a cluster peer it cannot dial

## Goal

moq-tokio exposes which URL schemes its build can dial, and moq-relay
refuses to start when a configured cluster peer uses a scheme it can't
dial (for example `iroh://` once the iroh backend is gone, or `ftp://`),
naming the peer, instead of retrying it forever. A dial of a scheme the
build can't carry fails with an error naming the scheme and, when a feature
would carry it, that feature.

## Plan

Decided 2026-10-31 (maintainer, the /quest-complete requeue session), a
follow-up of [moq-dev/moq#7345](https://github.com/moq-dev/moq/pull/7345),
which drops moq-tokio's iroh backend. A CodeRabbit thread there
([discussion](https://github.com/moq-dev/moq/pull/7345#discussion_r4432489908))
found the gap; the behavior predates that PR and covers every unsupported
scheme.

`rs/moq-tokio/src/client.rs` (`connect_inner`) picks a transport by scheme
under feature gates (`tcp`, `tls`, `unix`, `ws`/`wss`), and every other
scheme falls through to QUIC when it is compiled (raced with WebSocket when
that is too) or to WebSocket alone. Those reject what they can't carry
only at dial time: QUIC takes `http`, `https`, `moqt` and `moql`
(`rs/moq-tokio/src/quic/backend.rs`), WebSocket `http`, `https`, `ws` and
`wss` (`rs/moq-tokio/src/websocket.rs`), so a peer like `ftp://` fails each
attempt and is retried. So which schemes dial depends on the build's
features and on that fallthrough. Expose a dialability check from moq-tokio
(a function or method beside `Client`, derived from the same dispatch and
`cfg`s so it cannot drift and never rejects a scheme that dials today), and
check every peer in `Config::connect`
(`rs/moq-relay/src/cluster.rs`) against it at startup. A peer returned later
by an `api.url` endpoint can't fail startup; decide in the PR what it does
(recommended: log once and skip it rather than retry). Test a config naming
an unsupported scheme.

Decided 2026-11-07 (maintainer, /quest-complete), a follow-up of
[moq-dev/moq#8162](https://github.com/moq-dev/moq/pull/8162): ✅ fold the
dial-time error into this quest rather than a separate one, since both
read the same dispatch. Dialing `ws://` without `websocket` is reachable
and never says why: with `quic` on it fails in the QUIC backend with
`InvalidScheme` (`rs/moq-tokio/src/quic/backend.rs`), and with only `tcp`
or `uds` (such as moq-relay built `--no-default-features`) it falls through
to `NoBackend("no QUIC backend matched; this should not happen")`
(`connect_inner`). Drive both from the dialability check, so
`Client::connect` on an undialable scheme names the scheme and, when one
exists, the missing feature (`ws`/`wss` to `websocket`, `tcp`/`tls` to
`tcp`, `unix` to `uds`). Test both feature sets.

Public API: additive in moq-tokio. Wire: none.

## Related

- [Drop moq-auth's iroh transport](/quest/m4/auth-drop-iroh.md) - the other iroh leftover
