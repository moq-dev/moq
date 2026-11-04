# [M] A group a subscriber gives up on is always reported

## Goal

Every group a subscriber gives up on surfaces to the application: no group
vanishes because its stream was stopped or evicted before its header was read.
On the wire, the subscriber always learns the sequence of a group it abandoned.
In the API, `track::Subscriber`'s drop signal (today the crate-private
`poll_drop`, which the ordered reader and resume use to skip gaps) is public in
Rust, mirrored in `@moq/net` and the bindings, so an application, and the
receive-windows benchmark, can count losses instead of waiting forever.

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
  
  Doing both is fine if each is needed. Start with the maintainer's question:
  why does the subscriber stop or evict a stream before reading its header at
  all? If receive-window eviction can wait for the header, the wire half may
  shrink to that. Cover moq-transport too, or say why it differs.
- **Public drop signal.** Make the drop signal public, naming it to match the
  existing read methods (`recv_group` and friends; for example `recv_drop`
  returning the given-up run and its error), with the same shape in JS and the
  bindings. The ordered reader and resume keep using it internally. Its first
  consumers are the receive-windows benchmark and applications; the
  [API audit](/quest/m1/api-audit/README.md) rule for new surface is met.
- Stay consistent with the [drop set](/quest/m1/drop-set.md), which owns the
  per-cursor drop runs and their reasons. This quest reports what a cursor
  gives up; it doesn't change how a cursor judges a missing group.

Public API: additive (a public drop read in Rust, `@moq/net` and the bindings).
Wire: possibly lite-07 (SUBSCRIBE_DROP for a stopped group), decided here.

## Related

- [Drop set](/quest/m1/drop-set.md) - owns the per-cursor drop runs this reports
- [The receive-windows benchmark runs to the end](/quest/m0/nightly/receive-windows-evicted.md) - waits on this to count losses in overcommitted cells
