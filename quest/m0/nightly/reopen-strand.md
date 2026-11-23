# [M] A reader is not stranded by Unroutable after every link of its source's relay reopens

## Goal

In the nightly mesh replay, no reader starves after `reopen every link of b`:
a lite-07 relay whose neighbour's copy of a broadcast ended with the reopened
link holds a request for it, or tells its downstream the broadcast moved,
instead of answering Unroutable while it still shows it. `Nightly (Mesh replay
(6 relays))` passes without a `[[known]]` entry for the strand.

## Plan

Found by the triage of Nightly run
[44916717292](https://github.com/moq-dev/moq/actions/runs/44916717292)
(2026-11-21, at `9df4313aba`), the first night after
[moq-dev/moq#9231](https://github.com/moq-dev/moq/pull/9231), which held a
request for a claim a relay shows available and dropped the splice-stall
`[[known]]` entries without reproducing the original failure. The same
strand came back in cell `ring6-4`:
`reopen every link of b (2/2): unfed (starved) x4`, reader `d` on
`replay/b/0` to `replay/b/3`, starved 75 s to the end of the run. It passed
on 11-22. Every relay ran this checkout's build then (the roll to `new`
finished at 15:31:25; the event ran at 15:32:20), so this is not the
release-relay roll the `[[known]]` entry for `roll` covers.

What the run's `mesh-replay-ring6` artifact (kept 45 days) shows, all at
`info`:

- 15:32:30.144: relay `b` ends its neighbours' subscriptions to `replay/b/*`
  (`subscribed unauthorized`), as every reopen of its links does.
- 15:32:30.15 to 15:32:35: relays `c` and `e`, whose upstream copies just
  ended, answer relay `d`'s requests for `replay/b/0` with `unroutable`, and
  `d` answers its reader the same (`subscribed error ... err=unroutable`).
- Until `kill and restart b` at 15:33:35, `d`'s reader asks nothing again
  (its front strands a track refused Unroutable until its table moves) and
  reads nothing.

Next: rerun `just test mesh-replay --mesh test/mesh-replay/ring6.toml` with
`RUST_LOG=info,moq_net::model=debug,moq_net::route=debug` until a reopen of
every link of one relay strands a reader, and find which of `c`'s paths
answered Unroutable: `Node::forward` finding no neighbour that holds the
claim (the case #9231's `Router::awaits` holds) while the claim is not yet
known available again, or the front ending with its copy. Pin it in
`route::node::tests` or `model/front.rs` before fixing, and rerun
`/verify-failover`. The old quest's analysis is in git history at
`quest/m2/multicast/splice-stall.md` (deleted by #9231).

Public API: none expected. Wire: none expected.
