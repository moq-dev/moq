# [S] Fork noq

## Goal

`moq-dev/noq` exists, tracks n0-computer/noq, and publishes `moq-noq-proto`,
`moq-noq`, and `moq-noq-udp` on crates.io under those names. Published MoQ
crates depend on the fork's releases and `web-transport-noq` builds against
it. Every fork release records the parent commit it includes, so an advisory
against noq or Quinn can be checked against a MoQ release. The sync procedure
is written down in this repository's contributing docs.

## Plan

Fork now; upstream opportunistically. MoQ's transport features (per-stream
ACK progress, `RESET_STREAM_AT`, hierarchical send groups, the qmux crate,
probing, per-stream deadlines, receive timestamps) ship from the fork on
MoQ's schedule. A clean, general change is offered upstream when it is ready,
and a merged one leaves the carried set on the next sync. Nothing waits on a
review there.

Done so far:

- [moq-dev/noq#1](https://github.com/moq-dev/noq/pull/1) renames the packages
  and keeps the library paths, so a `noq_proto::` import becomes
  `moq_noq_proto::` and nothing else changes. `PARENT` names the upstream
  commit; `moq-ci`, `moq-release` (trusted publishing on a `v*` tag), and
  `moq-sync` are the fork's own workflows. Upstream's workflows stay in the
  tree, disabled in the repository settings, so a sync never conflicts on them.
- Sync is a weekly merge of upstream main opened as a PR, not a rebase: `main`
  is never force-pushed and the carried set is
  `git log --no-merges upstream/main..main`.
- [moq-dev/web-transport#398](https://github.com/moq-dev/web-transport/pull/398)
  switches `web-transport-noq` to `moq-noq`.

Left to do, once `moq-noq` 1.3.0 and `web-transport-noq` 0.4 are on crates.io:

- Pin the fork releases in this workspace's `Cargo.toml`. `moq-tokio` keeps
  upstream `noq-proto` for the `iroh` feature, whose controller factory types
  come from iroh's own noq; a build with `iroh` therefore compiles both stacks,
  and iroh connections keep upstream's controller until the fixes are upstream.
- Write the sync procedure into `CONTRIBUTING.md`: what the fork tracks, how a
  sync PR is reviewed, how an advisory against the parent is triaged, and the
  rule that a carried change lists its upstream PR or the reason it has none.

The bootstrap release is the rename alone. BBR3 as the default
`TransportConfig` controller was planned as the second carried change, but
noq-proto's `Pair` test harness never advances time for pacing delays and BBR3
paces from the first packet, so about twenty tests fail and one hangs. It is
offered upstream with a harness fix instead, as the
[upstream](/quest/next/quic/upstream.md) line's first proposal; MoQ already
selects BBR3 explicitly.

This lands on `main` as a breaking bump of `moq-tokio`, which re-exports
`web_transport_noq`, by maintainer decision on 2026-09-21.

## Related

- The single noq backend lets one fork carry every feature.
- [Release the stack](/quest/next/quic/release.md) - how fork releases reach
  published MoQ crates
- [Multipath spike](/quest/future/multipath-spike.md) - noq's multipath support
  is one reason noq was chosen as the parent
