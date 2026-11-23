# [S] moq-quic has every quinn fix it needs

## Goal

moq-quic, moq-sock's `udp`, moq-tokio's `quic::driver`, and moq-congestion's
Cubic and BBR3 carry every security and correctness fix quinn has landed since
the last sync, each ported with its regression test or recorded as not
applying. The fork ledger in `rs/moq-quic/README.md` says how far upstream was
checked.

## Plan

Recurring child of the [vendor sync](/quest/m3/vendor-sync/README.md), which
says how each run works and recurs. Decided 2026-10-27 (maintainer): the hard
fork never merges upstream, so checking upstream is recurring work.
Decided 2026-11-18 (maintainer, /quest-plan): compatibility with quinn's API
is not a goal, so moq-quic's API changes freely (as
[moq-dev/moq#9152](https://github.com/moq-dev/moq/pull/9152) did to its
streams), and a fix is ported to the fork's shape.

Last checked (2026-11-03): quinn `main` through
[`aea1e538`](https://github.com/quinn-rs/quinn/commit/aea1e5384927258887119fa1dc4fe3f23f1182c7)
and `0.11.x` through
[`77a51bdc`](https://github.com/quinn-rs/quinn/commit/77a51bdc5dc8b63bfba30429ab79fbb142bc8e2a),
and [quinn#2481](https://github.com/quinn-rs/quinn/pull/2481)'s BBR3 through
[`01b8848d`](https://github.com/quinn-rs/quinn/pull/2481/commits/01b8848dcb321eb17ad4d88dd6f84f28f45a0d6a),
all recorded in `rs/moq-quic/README.md`'s `Upstream` section with every
disposition. Start there, and record the new heads in the same place.

Checked 2026-11-14 (/quest-plan): quinn#2481 force-pushed to
[`0ebaf7e2`](https://github.com/quinn-rs/quinn/pull/2481/commits/0ebaf7e2c9932e432ef5e7caf916291b02d8277e),
a rebase onto `c7b46a96` whose BBR3 commits match `01b8848d`, so it has
nothing to port; recorded in the ledger. Of the nine `main` commits since `aea1e538`, check
[`aa61ce99`](https://github.com/quinn-rs/quinn/commit/aa61ce996b97b21c63731a84fac1346c2d70852b) (late
ACK-only packets are not spurious losses) against BBR3's spurious-loss undo
first. The ledger's `BBR3 references` entry holds the Google Linux BBRv3,
draft-ietf-ccwg-bbr, and QUICHE BBR3 heads; recheck them with quinn#2481 each
run, and record any divergence a change to them adds there.

Learned: a fix missing from the ledger is often already held, since the fork
reworked the same code (`stop` under `RESET_STREAM_AT`, the stale-accept
check under `handshake_timeout`, socket setup in `moq_sock::bind`), so check
the fork's code before porting. A fix to quinn's `Endpoint::client` or
`server` lands in `moq_sock::bind`, which the driver's test helpers bind
through. Most of `0.11.x` backports `main`; compare its commits by subject.
moq-congestion was extracted from moq-quic since the last run, so a
congestion or pacer fix now lands in `rs/moq-congestion`, while the ledger
stays in moq-quic's README.

Ruled out: quinn's batched `read_datagrams`
([`2697de1e`](https://github.com/quinn-rs/quinn/commit/2697de1e1314cb9f98502b7323acd5bdd836b582))
waits on the [driver lock profile](/quest/m9/perf/quic-lock-contention.md).

Public API: none expected; a ported fix that changes one says so. Wire: QUIC
behavior follows upstream's fixes.

## Required

- [Cut and publish the prepared breaking release](/quest/m3/release/cut.md) - the next cycle starts after the cut

## Related

- [Dependency audit](/quest/m3/audit-deps.md) - covers crates cargo-deny can see
