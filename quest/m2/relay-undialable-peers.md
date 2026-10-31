# [S] The relay refuses a cluster peer it cannot dial

## Goal

moq-tokio exposes which URL schemes its build can dial, and moq-relay
refuses to start when a configured cluster peer uses a scheme it can't
dial (for example `iroh://` once the iroh backend is gone, or `ftp://`),
naming the peer, instead of retrying it forever.

## Plan

Decided 2026-10-31 (maintainer, the /quest-complete requeue session), a
follow-up of [moq-dev/moq#7345](https://github.com/moq-dev/moq/pull/7345),
which drops moq-tokio's iroh backend. A CodeRabbit thread there
([discussion](https://github.com/moq-dev/moq/pull/7345#discussion_r4432489908))
found the gap; the behavior predates that PR and covers every unsupported
scheme.

`rs/moq-tokio/src/client.rs` (`connect_inner`) picks a transport by scheme
under feature gates (`tcp`, `tls`, `unix`, `ws`/`wss`, QUIC for the rest),
so which schemes dial depends on the build's features. Expose that set from
moq-tokio (a function or method beside `Client`, derived from the same
`cfg`s so it cannot drift), and check every peer in `Config::connect`
(`rs/moq-relay/src/cluster.rs`) against it at startup. A peer returned later
by an `api.url` endpoint can't fail startup; decide in the PR what it does
(recommended: log once and skip it rather than retry). Test a config naming
an unsupported scheme.

Public API: additive in moq-tokio. Wire: none.

## Related

- [Drop moq-auth's iroh transport](/quest/m2/auth-drop-iroh.md) - the other iroh leftover
