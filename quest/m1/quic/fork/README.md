# Hard-fork quinn into the monorepo as moq-quic

## Goal

MoQ's QUIC stack lives in `rs/`, forked from quinn-rs/quinn `main`:
`moq-quic` (the sans-IO core), quinn-udp in `moq-sock`, and quinn's async
layer and `web-transport-moq` in moq-tokio. Every MoQ QUIC path (moq-tokio
and moq-uring) runs on it, the `moq-noq*` dependencies are gone, and a QUIC change lands in the same
PR as the MoQ code that needs it.

Out of scope: iroh keeps upstream noq (the `iroh` feature still compiles it for
iroh's controller factory), and multipath, NAT traversal, and QAD are not
carried.

## Plan

Decided in the 2026-09-30 plan, with reasons, so later sessions don't reopen
them:

- **Hard fork, no merges.** The moq-dev/noq soft fork never merged upstream
  (its weekly sync job failed on a workflow-permission error), n0-computer/noq
  slowed to 5-7 commits a month in Aug-Sep, and n0's strict semver limits
  what we can upstream, not what a fork can change. Upstream fixes are
  cherry-picked by hand; offering our changes upstream is optional courtesy,
  not tracked work.
- **Base on quinn, not noq.** quinn-proto is 32.9k lines against noq-proto's
  58.5k, about 15k of which is multipath, NAT traversal, and QAD that MoQ
  never negotiates. Stripping those from noq was estimated at 2-3 weeks with
  loss-recovery risk, and would leave every quinn fix a hand port. quinn ships
  security fixes the day the advisory publishes (11 in 2026), and staying
  close to its code keeps those cherry-picks cheap. The cost is re-porting
  BBR3, the controller callbacks, and lazy stream slots, which is why MoQ
  moved to noq in the first place (#1706, #3342, #3789).
- **Multipath is deleted**, and with it the multipath spike.
- **In-tree**, so `just check` and CI cover the stack, one PR spans the core
  and its consumers, and `release` and `main` each carry their own copy instead of
  double-landing fixes on the 1.3 and 2.0 fork lines.
- **One new crate, named by role**: `moq-quic`. Decided 2026-10-06:
  quinn-udp becomes a `moq-sock` module, since both runtimes consume it, and
  quinn's async layer and `web-transport-moq` become moq-tokio modules,
  tokio-only, rather than `moq-quic-udp` and `moq-quic-tokio` crates. Each
  import's first commit stays verbatim from upstream for diffability. The
  imported code's README and license credit quinn and noq.
- **A break.** moq-tokio exposes `noq::Endpoint`,
  `noq::TransportConfig`, and `noq::Incoming` publicly, so the crate swap is
  a break.
- **Security folds into the fork.** Rebasing on quinn picks up the 2026
  advisories noq is missing; `release` stays on `moq-noq` until the next cut.
  The fork does not replace iroh's upstream noq.
- **moq-dev/noq is frozen**: security patches for `release` only, archived once
  no released MoQ crate depends on it. The other
  [QUIC quests](/quest/m1/quic/README.md) wait for this line and land
  in-tree.

This README's own work: delete moq-dev/noq's `moq-sync.yml` and mark its
README frozen. Advisory triage is documented in `rs/moq-quic/README.md`.
Every carried change in moq-dev/noq's `CHANGELOG-MOQ.md`
is either ported by a child quest or recorded as not applicable in the
[switch](/quest/m1/quic/fork/switch.md) PR.

## Required

- [Switch](/quest/m1/quic/fork/switch.md) - quinn's async layer and `web-transport-moq` join moq-tokio, both runtimes run on `moq-quic`, `moq-noq*` is gone, and relay memory matches `moq-noq`

## Related

- [Shard the endpoint](/quest/m1/quic/shard.md) - the first feature the fork makes possible
- [Remove moq-uring copies](/quest/m1/perf/uring-copies.md) - cheap copy removals on the new core
- [Redesign the QUIC I/O boundary](/quest/m2/quic-io-boundary.md) - the breaking perf rewrite, gated on a profile
