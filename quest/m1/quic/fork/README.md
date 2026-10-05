# Hard-fork quinn into the monorepo as moq-quic

## Goal

MoQ's QUIC stack lives in `rs/` as `moq-quic` (the sans-IO core),
`moq-quic-udp`, `moq-quic-tokio`, and `web-transport-moq`, forked from
quinn-rs/quinn `main`. Every MoQ QUIC path (moq-tokio and moq-uring) runs on
it, the `moq-noq*` dependencies are gone, and a QUIC change lands in the same
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
- **Named by role**: `moq-quic`, `moq-quic-udp`, `moq-quic-tokio`;
  `web-transport-moq` keeps its name. Each crate's README and license credit
  quinn and noq.
- **A break.** moq-tokio exposes `noq::Endpoint`,
  `noq::TransportConfig`, and `noq::Incoming` publicly, so the crate swap is
  a break.
- **Security folds into the fork.** Rebasing on quinn picks up the 2026
  advisories noq is missing; `release` stays on `moq-noq` until the next cut.
  [noq reassembly cap](/quest/m0/noq-reassembly-cap.md) only tracks iroh's
  upstream noq, which the fork does not replace.
- **moq-dev/noq is frozen**: security patches for `main` only, archived once
  no released MoQ crate depends on it. The other
  [QUIC quests](/quest/m1/quic/README.md) wait for this line and land
  in-tree.

This README's own work: delete moq-dev/noq's `moq-sync.yml`, mark its README
frozen, and document advisory triage in `rs/moq-quic/README.md`. `cargo audit`
cannot match renamed crates, so the triage is: watch quinn-rs/quinn's security
advisories and releases, check each fix against `moq-quic`, and port it with
its regression test. Every carried change in moq-dev/noq's `CHANGELOG-MOQ.md`
is either ported by a child quest or recorded as not applicable in the
[switch](/quest/m1/quic/fork/switch.md) PR.

## Required

- [Import quinn](/quest/m1/quic/fork/import.md) - quinn's three crates build and test in-tree as `moq-quic*`, verbatim at a recorded commit, with no consumer yet
- [Port BBR3](/quest/m1/quic/fork/bbr3.md) - the fork's corrected BBR3 and controller callbacks run on `moq-quic` as the default controller
- [Lazy stream slots](/quest/m1/quic/fork/stream-slots.md) - relay memory on `moq-quic` matches `moq-noq`
- [Switch](/quest/m1/quic/fork/switch.md) - `web-transport-moq`, moq-tokio, and moq-uring run on `moq-quic`, and `moq-noq*` is gone

## Related

- [Shard the endpoint](/quest/m1/quic/shard.md) - the first feature the fork makes possible
- [Remove moq-uring copies](/quest/m1/perf/uring-copies.md) - cheap copy removals on the new core
- [Redesign the QUIC I/O boundary](/quest/m2/quic-io-boundary.md) - the breaking perf rewrite, gated on a profile
