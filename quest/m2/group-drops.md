# [M] A group a subscriber gives up on is always reported

## Goal

Every group a moq-lite subscriber gives up on surfaces to the application: no
group vanishes because its stream was stopped or evicted before its header was
read. On the wire, the subscriber always learns the sequence of a group it
abandoned; moq-transport reports what its wire allows, and the PR records any
gap there.
In the API, `track::Subscriber`'s drop signal (today the crate-private
`poll_drop`, which the ordered reader and resume use to skip gaps) is public,
so the receive-windows benchmark and applications can count losses instead of
waiting forever.

## Plan

Planned 2026-11-04 with the maintainer, from
[moq-dev/moq#7838](https://github.com/moq-dev/moq/pull/7838). Under that
benchmark's overload policy, overcommitted cells hang. Sometimes the subscriber
drops a group without `recv_group` ever returning it: either the group's stream
was evicted before its header was read, or the group was evicted and so was the
FETCH that refetched it. No public API reports that group, so a reader that
paces on delivered-or-evicted groups stalls. The 4 MiB-frame cell hits this on
every run; the 256-stream cell sometimes does.

- ✅ **Milestone m1** (maintainer, 2026-11-04): it changes public API and
  possibly lite-07 wire, so it lands before the cut.
- ✅ **The wire mechanism is this quest's call, with numbers** (maintainer,
  2026-11-04). Compare:
  - **The publisher answers STOP_SENDING with SUBSCRIBE_DROP.** Lite already
    sends SUBSCRIBE_DROP on the control stream when a group fails at the
    publisher (`GroupServe::settle_failed`, `rs/moq-net/src/lite/publisher.rs`,
    versions with a track stream), but a subscriber's STOP_SENDING doesn't
    produce one.
  - **The subscriber reads a group's header before it stops or evicts that
    stream.**
  
  Doing both is fine if each is needed. The numbers come from the
  receive-windows benchmark's overcommitted cells (256 streams, 4 MiB frames,
  follow; `rs/moq-tokio/tests/integration/receive_credit.rs`), which
  [moq-dev/moq#7838](https://github.com/moq-dev/moq/pull/7838) skips until
  this lands: each must finish, with `delivered_frames` plus the reported
  drops accounting for every group sent. Compare their throughput and the
  control-stream bytes each mechanism adds. Start with the maintainer's question:
  why does the subscriber stop or evict a stream before reading its header at
  all? If receive-window eviction can wait for the header, the wire half may
  shrink to that. Cover moq-transport too, or say why it differs.
- **Public drop signal.** Make the drop signal public in Rust. Choose its
  shape here: a separate read named like the existing ones (for example
  `recv_drop` returning the given-up run and its error), or a drop variant on
  `recv_group`, the way container consumers already read `Frame | Gap | End`.
  Prefer folding it into an existing read if that reads as naturally. The
  ordered reader and resume keep using it internally. Its first consumer is
  the receive-windows benchmark. Mirror it in `@moq/net` and the bindings only
  for a named consumer (for example a player or container reader that skips
  gaps), per the API audit rule that new
  surface needs one; name it in the PR or leave the mirror out.
- **Order with the drop set ([moq-dev/moq#8476](https://github.com/moq-dev/moq/pull/8476)).** The drop set
  replaces `poll_drop`'s `Delivered` argument and the skipped record, and
  edits the lite-07 SUBSCRIBE_DROP wording. Publish the API against its
  final disposition record, so the public shape doesn't change twice.
- The drop set owns the per-cursor drop runs and their reasons. This quest
  reports what a cursor gives up; it doesn't change how a cursor judges a
  missing group.

Decided 2026-11-05 (maintainer, /quest-audit): ✅ this waits for
Drop set ([moq-dev/moq#8476](https://github.com/moq-dev/moq/pull/8476)), since both rewrite the lite-07
SUBSCRIBE_DROP draft wording and the drop signal's `Delivered` argument.

Decided 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests): ✅ key the drop signal by group start and land after
[time addressing's model](/quest/m2/time-addressing/model.md), so its public
API never ships sequence-keyed (rejected: landing first and rekeying with
the model). ✅ Drops surface as a `Gap { range, error }` item on
`track::Ordered`, which already skips gaps through `poll_drop`, mirroring
the container consumer's `Read::Gap` (`rs/moq-mux/src/container/consumer.rs`);
`recv_group` is unchanged (rejected: a separate `recv_drop` read; a drop
variant on `recv_group`). Answer the maintainer's question above first: if
receive-window eviction can wait for the header, take the no-wire path, and
remove this quest from [lite07-finalize](/quest/m2/lite07-finalize.md)'s
Required.

Open, for whoever starts this, with the maintainer: a `Gap`'s start-keyed
`range` doesn't say how many groups it covers once starts are sparse, yet
the Goal has readers count losses. Recommended: `Gap` also carries the
number of groups dropped, which the drop set's runs already know.

Public API: breaking for `track::Ordered` only. Its one read,
`poll_next_group` (and its async wrapper), returns
`Result<Option<Item>>` instead of `Result<Option<group::Consumer>>`, where
`Item` is a group or a `Gap { range, error }`; `Ok(None)` still ends the
read and `Err` still carries a track abort.
Callers match the new item, and a caller that ignores drops takes the group
arm; the PR's MIGRATE notes say so. `recv_group` and the unordered cursor are
unchanged, so a reader that needs drop accounting reads through `Ordered`.
`@moq/net` and the bindings mirror it only with a named consumer.
Wire: possibly lite-07 (SUBSCRIBE_DROP for a stopped group), decided here.

## Required

- [Address groups by timestamp](/quest/m2/time-addressing/model.md) - the drop range is keyed by start

## Related
- [The receive-windows benchmark](/quest/m1/receive-windows-evicted.md) - its overcommitted cells wait on this (skipped by [moq-dev/moq#7838](https://github.com/moq-dev/moq/pull/7838) until it lands)
