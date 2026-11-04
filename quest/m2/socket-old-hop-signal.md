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

The signal must be about the peer, not the grant. A session's publish
grant says only that it *may* publish under a prefix, not that the dialed
endpoint exists behind it. A marker covering an older session's grant would
also fail a dial to a peer that isn't online yet under that grant, which is
exactly the case that must keep waiting. So the grant only narrows where to
look, and the evidence has to name the endpoint.

Recommended design, to settle in this quest:

- **Find the endpoint.** When a dial's interest in `<prefix>/.socket/<peer>`
  reaches a relay, the relay asks each of its older sessions whose publish
  grant covers that path, with an ANNOUNCE_REQUEST naming the path, held
  open while the dial waits so a peer that comes online later is seen. Such a
  request names the hidden segment, so a lite-06 peer does announce its
  endpoint (#7888's round-1 review response,
  `rs/moq-net/src/lite/publisher.rs`). A session that may publish nothing
  (a viewer) is never asked.
- **Tell the dialer.** If an older session announces the endpoint, the relay
  sends a marker for that exact path on the wire: the endpoint exists behind
  an older hop. `D`'s endpoint fails the dial with `Unsupported`, and only
  for that path. With no answer, nothing is marked, and the dial waits as
  for an absent peer. The marker tracks that announcement: it goes when the
  older session unannounces the endpoint or ends, whichever comes first.
- **An announced route wins.** While any route announces the endpoint (for
  example, a current session overlapping an older one during a reconnect),
  the dial uses it and ignores the marker.
- **Across a cluster.** Relay-to-relay links ask for everything, not for a
  dialer's narrow interest, so the relay holding the older session may never
  see the dial, and a dial sends no SUBSCRIBE until the endpoint is
  announced. Settle how the dialer's narrow interest reaches the holding
  relay (for example, cluster links forwarding narrow interest under
  `.socket`), and how markers cross cluster links.
- **Where the marker rides.** Its own announce-stream message, or a flag on
  the [route layer](/quest/m1/cluster-routing/routes/README.md)'s messages if
  that layer has landed. Choose whichever leaves the draft simpler.
- **Not chosen:** routing the dial once the probe finds the endpoint. The
  peer can't tell that its relay probes, so it keeps failing its own dials
  and accepts (#7888), and the session would connect from one side only.

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
across a cluster hop. A dial to an absent peer still waits, including one
whose path lies under another lite-06 session's publish grant. A viewer-only
lite-06 session is never asked. A marker goes when the older session
unannounces the endpoint, and a current route announcing the same endpoint
wins over it. A peer opted into hidden routes over lite-06
(`an_older_hop_opted_into_hidden_connects`) still connects.

Public API: none expected; dials return the existing `Error::Unsupported`.
Wire: a new per-path marker on the current moq-lite wip.

## Related

- [Sockets](/quest/m1/socket/README.md) - the dial and accept this reports on, and #7888's first-hop check
- [Routes and announces](/quest/m1/cluster-routing/routes/README.md) - the route layer a marker may ride, and how interest crosses cluster links
