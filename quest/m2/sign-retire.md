# [S] Per-frame signing leaves moq before the cut

## Goal

The cut publishes no per-frame signing (moq-sign-00): the signing wrappers,
their JS twin, the draft, and its vectors leave moq, the endpoint key the
cluster route key uses stays, and the docs say what moq-crypto publishes.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
The signing line ([moq-dev/moq#7092](https://github.com/moq-dev/moq/pull/7092),
[moq-dev/moq#7870](https://github.com/moq-dev/moq/pull/7870),
[moq-dev/moq#7702](https://github.com/moq-dev/moq/pull/7702)) was built for
the mesh, where content would be signed by default. The mesh moved downstream
([moq-dev/moq#8756](https://github.com/moq-dev/moq/pull/8756)). No crate uses
`moq_crypto::sign`'s broadcast, track, or group wrappers; the only Rust use of
`sign` is `SecretKey` for `--cluster-route-key` (`rs/moq-relay/src/cluster.rs`),
and nothing outside `js/crypto` imports `@moq/crypto`'s signing.
`draft-lcurley-moq-sign` was never submitted to the datatracker, and
`release` has no sign code. Yet moq-crypto is a required dependency of the
published moq-quic, moq-tokio, and moq-auth, with default features
`["sign", "e2ee"]`, so the cut would publish about 1.5k lines of Rust signing
API, and `doc/lib/rs/index.md` wrongly calls moq-crypto "Workspace-only".

e2ee stays: its draft (rev 00) and `moq-e2ee` are already published.

Delete `rs/moq-crypto/src/sign/` except `SecretKey` and
`PublicKey` (kept behind a feature that does not pull in `moq-net`), its
bench, `js/crypto`'s sign wrappers, and `drafts/draft-lcurley-moq-sign.md` with
`moq-sign-00.{json,ts}`; correct `doc/lib/rs/index.md`, `doc/lib/js/index.md`,
and `doc/concept/standard.md` to say e2ee is published and experimental.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ (a) delete per-frame signing and keep the endpoint key (rejected: (b) hand it downstream; (c) publish it at the cut).

Public API: (a) removes moq-crypto's and @moq/crypto's signing wrappers,
neither yet published. Wire: (a) withdraws the unpublished moq-sign-00 draft.

