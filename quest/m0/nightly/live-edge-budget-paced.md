# [L] A session's live-edge budget never sheds a frame a paced reader is owed

## Goal

`quic_sim::many_groups_carry_bulk_transfers_losslessly` (Nightly's `Tests (rs
quic-sim)`, `just rs quic-sim`) passes on any seed: a client session past its
`live_edges` budget sheds only frames no reader is owed, and stays bounded by
pausing ingest rather than by losing frames its paced readers have yet to take.

## Plan

Nightly run [45035580806](https://github.com/moq-dev/moq/actions/runs/45035580806)
failed `data4 frame 1: evicted`. It replays on `main` at `a2e356dfd0`:

```sh
cd rs && MOQ_SIM_SEED=13746849096869201041 \
  CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=true \
  cargo nextest run --profile ci --locked --release -p moq-relay --features test-support \
  --test integration --run-ignored only -E 'test(/^quic_sim::many_/)'
```

With `limits.client.live_edges = 0` in the test's cluster options, the same
seed passes in 11 s. No QUIC stream is evicted. The cause:

- The relay ingests the publisher's 100 groups on one client session, whose
  `live_edges` budget is 256 MiB by default
  ([moq-dev/moq#9308](https://github.com/moq-dev/moq/pull/9308)).
- Ingest runs each group up to `pace::window`, the stream's receive window
  (4 MiB by default), ahead of the group's fastest reader. Nothing bounds that
  across a session's groups, so 100 paced groups may hold 400 MiB that no
  reader took. Instrumented, they held about 2.6 MiB each at the failure.
- Past the budget, `cache::Edges::reclaim` calls `TrackState::shed_edge_all`
  on the oldest edge. That sheds frames the relay's reader has not taken yet,
  and aborts a copy still receiving a frame (`group::Handle::shed_all`).
  Either way the downstream reader reads `Evicted`, which the wire carries as
  0x35 to the subscriber.

Shedding only the frames readers took first is necessary but not enough.
Branch `fix/live-edge-shed-before-abort` (head `ab96065ebf`) does it: it sheds
the frames each edge's fastest reader took, or everything of an edge no reader
holds, before any frame a reader is owed. The seed still fails, since frames
ahead of active readers alone fill the budget. Reuse that branch for the
shedding order.

The budget needs a way to stay bounded without shedding owed frames. Options,
pending the maintainer's pick under Waiting:

- **Pace ingest to the budget (recommended).** While its session is past
  `live_edges`, a group's ingest with a reader behind stops reading ahead,
  as `poll_paced` does past its window, and resumes once a reader takes frames
  and the shed brings the session back under. Stream credit then pushes back
  to the publisher, as for any slow reader, so a hostile publisher still can't
  make the relay hold more. Shedding keeps its current order only for groups
  no reader holds and for a copy left receiving a frame while ingest is paused.
  Check the credit side with `/verify-credit`: many streams paused at once must
  still fit the half-window allowance from
  [moq-dev/moq#9210](https://github.com/moq-dev/moq/pull/9210).
- **Split one session-wide ahead allowance among paced groups**, by slot as
  `Pacing::allowance` splits credit. This bounds owed bytes by design, but
  shrinks every group's read-ahead as the track count grows, even when the
  budget has room.
- **Leave the design and set `live_edges = 0` in the test.** This is cheapest,
  but a relay at its defaults still loses frames for a publisher with many
  concurrent bulk groups, which the test exists to catch.

Public API: none expected. Wire: none.

## Waiting

- Maintainer: pick how a session past `live_edges` stays bounded without
  shedding frames a paced reader is owed: pace ingest to the budget
  (recommended), split a session-wide ahead allowance by slot, or set
  `live_edges = 0` in the test. Check: the pick is recorded in this Plan.

## Related

- [Pool pinned debt](/quest/m1/pool-pinned-debt.md) - the pool-side bound
  on live edges ([moq-dev/moq#9422](https://github.com/moq-dev/moq/pull/9422)),
  which leaves the session budget's shedding as is
