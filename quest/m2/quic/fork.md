# [S] Fork noq

## Goal

`moq-dev/noq` exists, tracks n0-computer/noq, and publishes `moq-noq-proto`,
`moq-noq`, and `moq-noq-udp` on crates.io under those names. `dev` depends on
the fork and `web-transport-noq` builds against it. Every fork release records
the parent commit it rebased onto, so an advisory against noq or Quinn can be
checked against a MoQ release. The sync procedure is written down in this
repository's contributing docs.

## Plan

Fork now; upstream opportunistically. MoQ's transport features (per-stream
ACK progress, `RESET_STREAM_AT`, hierarchical send groups, the qmux crate,
probing, per-stream deadlines, receive timestamps) ship from the fork on
MoQ's schedule. A clean, general change is offered upstream when it is ready,
and a merged one is dropped from the carried set on the next rebase. Nothing
waits on a review there.

- Fork n0-computer/noq into moq-dev/noq. Rename the packages in their
  manifests to `moq-noq-proto`, `moq-noq`, and `moq-noq-udp` so a published
  crate never impersonates the parent; keep the library paths and public
  types as they are so a `noq_proto::` import becomes a `moq_noq_proto::`
  import and nothing else changes.
- Carry a `PARENT` file naming the upstream commit, and a CI job that
  attempts the rebase onto upstream main weekly and opens a PR with the
  result, so drift is visible before it is expensive.
- Publish the first release with two carried changes: the rename, and BBR3
  as the default congestion controller (MoQ's default already; offering it
  upstream is [the first proposal](/quest/m2/quic/upstream.md)). Switch
  `web-transport-noq` in moq-dev/web-transport to it.
- Pin the fork releases in this workspace's `Cargo.toml`. `dev` may point at a
  git tag between releases; `main` and every published crate pin crates.io
  versions, and the [release quest](/quest/m2/quic/release.md) owns that rule.
- Write the sync procedure into `CONTRIBUTING.md`: which upstream releases
  the fork tracks, who rebases, how an advisory against the parent is
  triaged, and the rule that a carried change lists its upstream PR or the
  reason it has none.

## Related

- [One QUIC backend](/quest/m1/quic-one-backend.md) - the reason a single
  fork can carry every feature
- [Release the stack](/quest/m2/quic/release.md) - how fork releases reach
  published MoQ crates
- [Multipath spike](/quest/m3/multipath-spike.md) - noq's multipath support
  is one reason noq was chosen as the parent
