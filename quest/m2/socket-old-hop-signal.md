# [L] A socket dial learns its peer is behind an older hop

## Goal

A socket dial to a peer whose endpoint an older moq-lite hop withholds fails
with `Unsupported` instead of waiting, while a dial to a peer that isn't
online yet keeps waiting. The relay holding the older session tells the
dialer on the wire. Dialers and relays on versions without that signal keep
today's wait.

## Plan

Planned 2026-11-04 with the maintainer, from the limit
[moq-dev/moq#7888](https://github.com/moq-dev/moq/pull/7888) documented.
Milestone m2 (maintainer).

Where it gets lost: an endpoint's `.socket/<id>` broadcast is hidden, and
before lite-07 ANNOUNCE_REQUEST has no hidden opt-in. When a peer `P`
reaches its relay `R` over a moq-lite-06 (or older) client session whose
publish half isn't opted into hidden routes, `R` never learns `P`'s
endpoint. Since #7888, `P` sees this from its own session version
(crate-private `HiddenWithheld` in `rs/moq-net/src/model/origin.rs`) and
fails its own dials and accepts. A lite-07 dialer `D` gets nothing: it waits
for `P`'s announcement as for a peer not online yet. `R` can't tell the two
apart either, since it never sees `P`'s hidden path. What `R` does know is
which of its sessions are older moq-lite with a publish half not opted into
hidden, and each one's publish scope from its grants.

Recommended design, to settle in this quest:

- **What tells the dialer.** `R` sends a "hidden routes withheld" marker on
  its announce stream for each such session, covering that session's
  publish scope where it meets the request's prefix, and withdraws it when
  the session ends. Relays forward markers like routes, so they cross
  cluster links. `D`'s endpoint fails a dial with `Unsupported` while a
  marker covers the peer's `.socket/<id>` path and no route serves it. An
  announced route always wins over a marker.
- **Scope by publish grant.** A session that may publish nothing (a viewer)
  marks nothing, so one old viewer can't fail every dial on a relay. That
  is why the signal can't reuse #7888's coarse, origin-wide count, which is
  safe only for an endpoint's own hop. A session with an unrestricted
  publish scope marks everything it may publish; the PR records that cost.
- **When to judge.** The dial judges only once the dialer's announce sync
  has settled, so it never misses a marker that arrives with the snapshot.
- **Where it rides.** As its own announce-stream message, or as a flag on
  the [route layer](/quest/m1/cluster-routing/routes/README.md)'s messages
  if that layer has landed. Choose whichever leaves the draft simpler, and
  say how markers merge across relays.

Weigh this alternative before choosing: `R` forwards a dialer's narrow
interest to its older sessions, since a lite-06 ANNOUNCE_REQUEST naming the
`.socket/<id>` segment does list the hidden route (#7888's round-1 review
response, `rs/moq-net/src/lite/publisher.rs`). The dial would then connect
with no wire change, but `P` can't tell that its relay forwards interest,
so `P`'s own refusal from #7888 would stay. Pick it only if that one-sided
result is acceptable.

Versions and compatibility:

- The signal goes in whichever moq-lite wip is current when this lands
  (lite-07-wip before its cut, per the
  [freeze policy](/quest/m1/lite07-finalize.md), otherwise the next wip),
  with its draft text in the same PR.
- A dialer whose relay doesn't send it, or whose own session predates it,
  waits as it does today. Published versions are unchanged.
- moq-transport hops carry no marker, so a dial across one waits as today.
  No IETF-only surface is added.
- Rust, `@moq/net`'s socket and moq-ffi behave alike. `doc/concept/socket.md`
  and the `Endpoint` rustdoc replace the "waits as for an absent peer"
  limit.

Tests, in the relay harness (`rs/moq-net/tests/integration/socket.rs`): a
lite-07 dial to a peer behind a lite-06 hop fails `Unsupported`, including
across a cluster hop. A dial to an absent peer still waits. A viewer-only
lite-06 session marks nothing. A peer opted into hidden routes over lite-06
(`an_older_hop_opted_into_hidden_connects`) still connects.

Public API: none expected; dials return the existing `Error::Unsupported`.
Wire: a new marker on the current moq-lite wip.

## Related

- [Sockets](/quest/m1/socket/README.md) - the dial and accept this reports on, and #7888's first-hop check
- [Routes and announces](/quest/m1/cluster-routing/routes/README.md) - the route layer a marker may ride
