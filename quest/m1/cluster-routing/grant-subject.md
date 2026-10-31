# [M] Grants carry an issuer-qualified subject

## Goal

`moq_net::auth::Grant` and moq-auth carry the verified token subject,
qualified by its issuer, in Rust and JS. A scoped token without a subject is
refused, and so is a token added to a session (refresh or `auth().add`)
whose issuer-qualified subject differs from that of the session's current
grants. The routing reference model no longer exempts a node's own proven
identity, and a fixture shows a forger that declares the victim's id as its
own captures nothing outside its scope. No wire change.

## Plan

Decided 2026-10-27 (maintainer): split out of
[Routes and announces](/quest/m1/cluster-routing/routes.md) as a ready
quest, since the subject is the key routes and
[federation](/quest/m1/cluster-routing/federation.md) file scoped identities
under (`hash(subject, id)`, decided 2026-10-15), and it lands without the
route layer.

- Today `moq_auth::Claims` reads and drops `sub` and `iss`
  (`rs/moq-auth/src/wire.rs`), and `Grant` (`rs/moq-net/src/auth.rs`) holds
  only patterns and expiry. Mirror the change in `js/auth` and `js/net`.
- Fail loud, per the 2026-10-15 decision in routes. Report the public API
  change to `Grant` and to the auth error surface.
- The routing contracts' reference model still exempts a proven identity
  (the `proved` map in `rs/moq-net/tests/integration/routing_contracts/model.rs`).
  Drop that exemption, add the forger that declares the victim's id as its
  own, and fix the comment on
  `forged_identity_hijacks_claims_outside_the_forgers_scope`
  (`routing_contracts/mod.rs`), which still says the forger's own proven
  identity stays bare: since 2026-10-26 every identity a scoped connection
  sends is subject-qualified, its own included.
- Update `doc/bin/relay/auth.md` for the refusals.

Public API: `Grant` gains the issuer-qualified subject in Rust and JS, and
scoped tokens without a subject are refused. Wire: none.
