# [S] A scoped peer's lite-07 session exchanges claims instead of closing Unauthorized

## Goal

A lite-07 endpoint declares a Node ID in SETUP only when the grant it admits
for that session covers the whole tree. A peer accepted with a link and a
scoped grant at the root, such as publish and subscribe `["tenant-a"]`,
exchanges claims for its prefixes as it does on lite-06, instead of having
its session closed with `Error::Unauthorized`.

## Plan

Reproduced 2026-11-21 with a scratch moq-relay integration test: a CDN
accepting a lite-07 dial with link `upstream`, root `""`, and publish and
subscribe `["tenant-a"]` never routes `tenant-a/cam`. The same grant passes
over lite-06, and over lite-07 with no link. The dialer retries forever, so
it shows only in logs. This is the local-cluster-dials-a-CDN setup that
[federation](/quest/m3/cluster-routing/federation.md) builds on.

The chain:

- `Cluster::scope_session` widens the session's handles to every prefix so
  in-band AUTH can regrant (`rs/moq-relay/src/cluster.rs:1601-1602`), and
  `Cluster::publisher` skips the stats exclusion for a linked token
  (`cluster.rs:1738`).
- `origin::Consumer::node_id` sees whole handles and declares a Node ID
  (`rs/moq-net/src/model/origin.rs:7794-7804`, decided at
  `lite/session.rs:270`).
- `RouteStream::poll_setup` checks the real AUTH permit and closes
  (`lite/subscriber/route_stream.rs:256`), as the draft mandates
  (`drafts/draft-lcurley-moq-lite.md:512`).

Fix it in moq-net, not moq-relay: any server that widens its handles for
in-band AUTH hits this. Recommended: decide the Node ID from the admitted
grant rather than the handles, which the session can do because routing
already waits for the AUTH answer. Cover a later regrant that widens to or
narrows from the whole tree, and say in the PR whether the draft's rule
needs rewording. `moq auth serve` reaches it too, through a certificate rule
carrying a link and scoped paths at the root (`rs/moq-auth/src/serve.rs:130-137`).

Test: the integration test above, dialing in both directions.

Caused by: likely [moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987),
whose Node ID check against the AUTH permit meets the widened handles of
[moq-dev/moq#5282](https://github.com/moq-dev/moq/pull/5282); confirm by
diff.

Public API: none expected. Wire: none, at most draft wording.
