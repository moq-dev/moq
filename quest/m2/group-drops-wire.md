# [M] A moq-lite subscriber always learns which group it abandoned

## Goal

No group a moq-lite subscriber gives up on vanishes because its stream was
stopped or evicted before its header was read: the subscriber always learns
the ID of a group it abandoned. moq-transport reports what its wire allows,
and the PR records any gap there. Whether this needs a lite-07 wire change is
settled before lite-07 freezes.

## Plan

Split 2026-11-14 from [the public drop signal](/quest/m2/group-drops.md) (the
m2 triage before the cut, under /quest-afk, for the maintainer's review):
✅ the wire half reads no group key, so it starts now, and
[lite-07's freeze](/quest/m2/lite07-finalize.md) waits on it instead of on
the public API (rejected: keeping one quest, which holds this wire question
until the Rust model lands).

Planned 2026-11-04 with the maintainer, from
[moq-dev/moq#7838](https://github.com/moq-dev/moq/pull/7838). Under that
benchmark's overload policy, overcommitted cells hang. Sometimes the subscriber
drops a group without `recv_group` ever returning it: either the group's stream
was evicted before its header was read, or the group was evicted and so was the
FETCH that refetched it. The 4 MiB-frame cell hits this on every run; the
256-stream cell sometimes does.

✅ **The wire mechanism is this quest's call, with numbers** (maintainer,
2026-11-04). Compare:

- **The publisher answers STOP_SENDING with SUBSCRIBE_DROP.** Lite already
  sends SUBSCRIBE_DROP on the control stream when a group fails at the
  publisher (`GroupServe::settle_failed`, `rs/moq-net/src/lite/publisher.rs`,
  versions with a track stream), but a subscriber's STOP_SENDING doesn't
  produce one.
- **The subscriber reads a group's header before it stops or evicts that
  stream.**

Doing both is fine if each is needed. Start with the maintainer's question:
why does the subscriber stop or evict a stream before reading its header at
all? If receive-window eviction can wait for the header, take the no-wire
path and remove this quest from
[lite07-finalize](/quest/m2/lite07-finalize.md)'s Required.

Take the numbers from the receive-windows benchmark's overcommitted cells (256
streams, 4 MiB frames, follow; `rs/moq-tokio/tests/integration/receive_credit.rs`):
each must account for every group sent as delivered or abandoned, counted
inside moq-net until [the public drop signal](/quest/m2/group-drops.md) lets
the benchmark count them itself. Compare their throughput and the
control-stream bytes each mechanism adds. Cover moq-transport too, or say why
it differs. A wire rule applies to `js/net`'s publisher and subscriber in the
same PR.

The drop set owns the per-cursor drop runs and their reasons
([moq-dev/moq#8476](https://github.com/moq-dev/moq/pull/8476)); this changes
only whether an abandoned group is known, not how a cursor judges a missing
one.

Public API: none. Wire: possibly lite-07-wip (SUBSCRIBE_DROP for a stopped
group), decided here.

## Related

- [The public drop signal](/quest/m2/group-drops.md) - surfaces what this makes known to the application
