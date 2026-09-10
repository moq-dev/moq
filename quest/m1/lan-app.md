# [S] LAN discovery app id

## Goal

Two unrelated MoQ applications on one network never discover each other. Every
LAN advertisement names an application; a peer from another application is
never resolved or reported, and the membership proofs are bound to the
application, so one secret reused across two applications still yields two
meshes. moq-cli and moq-relay share one default, so they find each other with
no configuration.

Non-goals: hiding the record (mDNS is multicast in the clear either way) and
any change to how a discovered peer is dialed.

## Plan

`rs/moq-tokio/src/mdns.rs` registers every process under one DNS-SD type,
`_moq._udp.local.`, and tells groups apart only after resolving, by whether the
HMAC proof verifies. Two applications with no secret therefore mesh with each
other, and two with different secrets still resolve and log every foreign
record.

- `mdns::App`: a newtype over 1..=63 lowercase ASCII letters, digits, and
  hyphens, with `FromStr` and `Display`, refusing anything else at parse time.
  `impl Default` returns `default`, the name both binaries advertise under.
  `Config::new(app, port)` replaces `Config::new(port)`; moq-tokio is
  unpublished, so no shim.
- Partition: register the service under the DNS-SD subtype
  `_<app>._sub._moq._udp.local.` (RFC 6763 section 7.1) and browse that
  subtype, so the daemon only ever resolves this application's records.
  mdns-sd 0.20 supports it: the subtype rides `ServiceInfo::new`'s type
  argument and `browse` takes the same string. `_moq._udp` stays the parent
  type, so `dns-sd -B _moq._udp` still lists every MoQ process on the network.
- Bind: `Identity` gains the app and both proofs cover it, so a record cannot
  be replayed under another application's subtype and the same secret under
  two apps never verifies. Extend `proofs_are_bound_to_one_listener`.
- Surfaces: moq-cli gains `--cluster-lan-app` (`MOQ_CLUSTER_LAN_APP`) beside
  `--cluster-lan-secret` in `rs/moq-cli/src/cluster.rs`; moq-relay gains
  `[cluster.lan] app` (`--cluster-lan-app`) in `LanConfig`. Both default to
  `default` and document that an application built on the library picks its
  own name. Update `doc/bin/relay/cluster.md` and `doc/bin/relay/config.md`;
  `doc/bin/cli.md` does not mention `--cluster-lan` at all, so add the flags
  there while reconciling against `--help`.
- Test: there is no mDNS test today. Add one that runs two `Discovery`
  instances on the host under different apps and asserts neither reports the
  other, then two under the same app and secret that do. Skip cleanly when no
  interface can announce, the way `ANNOUNCE_TIMEOUT` already fails, so CI
  without multicast stays green rather than silently passing.

Branch from `dev`; mDNS exists only there.

## Related

- [One LAN mesh](/quest/m1/lan-mesh.md) - consolidates the flag surface this adds into the relay's `ClusterConfig`
