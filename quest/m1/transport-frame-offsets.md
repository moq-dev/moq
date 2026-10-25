# [M] The transport may start a group mid-way

## Goal

A moq-lite hop may request and deliver a group starting at any frame k the
requester needs, whether or not it still holds the frames below k (route
continuation, hole fills, and front-eviction refills after the front was
evicted, with a frame-precise end), so a relay never refetches frames it does
not need. Applications still only ask for whole groups: no public API
starts at a frame. Rust and `@moq/net` both accept and cache such a tail.

## Plan

Decided 2026-10-25 (maintainer): remove frame-N starts from the API, as
[moq-dev/moq#6465](https://github.com/moq-dev/moq/pull/6465) did, but allow
them in the transport as an optimization.

- Draft: amend lite-07-wip so a subscriber may send any nonzero frame start,
  replacing "MUST send 0 unless it already holds every frame below this
  index". A front-eviction refill needs frames k and up after the frames
  below k were evicted, so holding the prefix cannot be the condition. Keep
  the `Frame End` rule.
- Widening is relay policy, not a wire rule: a relay asked for frame k of a
  group it holds none of widens the upstream request to frame 0 and cuts
  locally, so the cache stays usable by
  whole-group readers and a compressed group can inflate.
- A partial copy is the same group, never a new identity: `covering_group`
  already serves only copies that cover the start. A copy cut short at the back
  is marked incomplete so it never answers a whole-group fetch.
- JS accepts a GROUP with `frameStart > 0` (numbering from it, as Rust's
  `group::Producer::start_at` does) instead of refusing it as a protocol
  violation.
- A moq-transport stream that starts mid-group with no head is still dropped
  (2026-10-22 decision, reaffirmed 2026-10-25).
- The rule in `rs/moq-net/AGENTS.md` that banned partial groups was deleted
  2026-10-25.

Bound the cost a hostile peer can create: many distinct tail starts for one
group must not multiply its copies; coalesce to one copy per group.

Public API: none. Wire: lite-07-wip draft text only; the fields exist.

## Related

- [Front eviction](/quest/m1/unbounded-groups/front-eviction.md) - refills a front hole with a frame-precise range
