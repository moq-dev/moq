# [L] One LAN mesh

## Goal

A `moq --cluster-lan` process and a `moq-relay` with `[cluster.lan]` on the
same network mesh with each other, in both directions, through one
implementation. A LAN peer authenticates with its mDNS credential and is never
handed `cluster.token`; the secret is optional for both, and without one the
mesh is open and documented as such.

## Plan

Two meshes share the `_moq._udp` service today and cannot connect. The CLI
(`rs/moq-cli/src/cluster.rs`) advertises a fingerprint and no node, dials
`/.cluster/<credential>` with the fingerprint pinned, and attaches one
unauthenticated origin. The relay (`rs/moq-relay/src/cluster.rs`) always
advertises a node, skips any peer without one, dials the node URL with
`?jwt=<cluster.token>` under ordinary TLS trust, and never reads
`Peer::credential` or `Peer::fingerprint`. A CLI dialing a relay lands as an
anonymous client at the node URL and is refused unless `--auth-public` happens
to cover it.

One implementation: the relay's `Cluster`.

- moq-cli depends on the moq-relay library; its `cluster-lan` feature becomes
  `moq-relay/cluster-lan`. `MoqSide.cluster` nests `moq_relay::ClusterConfig`,
  so the CLI's `--cluster-*` flags are the relay's, LAN and WAN alike, and the
  stages publish and subscribe on the cluster's origin (built once at
  construction). Delete `rs/moq-cli/src/cluster.rs`.
- Advertise: the listener's fingerprint whenever its certificate was
  generated, the node URL when one is configured, and at least one of them
  (the `UnboundSecret` rule). A relay without `--cluster-node` advertises its
  listen port, the way the CLI does, so the `--cluster-lan` needs
  `--cluster-node` start-up check goes away.
- Dial: `Peer::urls()` in order, node first, on the path
  `/.cluster/<credential>`, with the advertised fingerprint pinned. The lower
  id dials. Never `?jwt=`; `cluster.token` is for static and gossip peers only.
  The request path needs moq-lite-05 or any moq-transport version, so the
  CLI's `validate_versions` moves with the dial.
- Accept: `Connection::authenticate` recognizes `/.cluster/<credential>`,
  verifies it with `Discovery::verify_credential` (constant time), and grants
  the cluster-peer scope a `cluster.token` would, recording the hop. Every
  other path stays on the JWT, mTLS, and public-prefix path. A `/.cluster`
  request on a relay without LAN discovery is refused, not routed.
- Secret: optional for the relay too, since a LAN dial no longer carries the
  token; `doc/bin/relay/cluster.md` inherits the CLI's warning that an open
  mesh admits anyone who can reach the listener. Drop the relay's
  secret-required start-up check and reconcile
  [cluster flags](/quest/m2/cluster-flags.md), which lists both removed checks.
- Docs: `doc/bin/relay/cluster.md`, `doc/bin/relay/config.md`, and the CLI
  page describe one mesh; the CLI page names the WAN flags it gains.
- Test: an in-process test meshes a node-advertising cluster with a
  fingerprint-advertising one and asserts a broadcast crosses in each
  direction. A smoke recipe runs `moq import --cluster-lan` beside `moq-relay`
  with `[cluster.lan]` and watches from the relay.

Branch from `dev`.

## Related

- [`moq relay`](/quest/m2/moq-relay-subcommand.md) - the second place the CLI hosts the relay library
