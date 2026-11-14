# [S] Mesh grants scope nodes and services apart

## Goal

A mesh member's grant publishes `mesh/node/<id>/**` and the two service
patterns `mesh/service/*/.server/<id>` and `mesh/service/*/.client/<id>`, in
place of `mesh/<id>/**`. A service named after another member can no longer
put `.server/<other>` under its own grant and answer as that member. The
shape lands before the cut publishes `moq_auth::mesh` for the first time, so
no enrolled device ever holds the old grant from a release.

## Plan

Split from the [Endpoint API](/quest/m3/mesh/endpoint.md) on 2026-11-12
(maintainer, /quest-audit before the cut): ✅ the grant shape lands in m2 on
its own (rejected: landing with the Endpoint after the cut, with the
coordinator issuing both shapes for one release). `rs/moq-auth/src/mesh`
exists on `main` and not on `release`, so there is nothing to migrate and no
dual-grant rollout.

The shape was decided 2026-11-07 (maintainer, /quest-complete, from the
review of [moq-dev/moq#8214](https://github.com/moq-dev/moq/pull/8214)):
members own `mesh/node/<id>/**` and each service lives at
`mesh/service/<service>` (rejected: `mesh/.service/<service>`; keeping
`mesh/<service>` and refusing service names shaped like an endpoint ID). A
service name is one path segment, so `*` in the grant covers it.

- `moq_auth::mesh`'s coordinator issues the new patterns
  (`rs/moq-auth/src/mesh/coordinator.rs`), with its docs
  (`rs/moq-auth/src/mesh/mod.rs`) and contract tests.
- `doc/bin/relay/auth.md`'s mesh enrollment section names the new grant.
- moq-crypto's and `@moq/crypto`'s sign examples and tests that use
  `mesh/<key>/agent` paths move to `mesh/node/<key>/agent`.
- A hosted coordinator implementing the JSON contract issues the same grant;
  the PR's Impact section says so. Applications built on `main` re-enroll.

Public API: the grant a mesh coordinator issues changes; no Rust or JS
signature does. Wire: none.

## Related

- [Mesh enrollment is bounded](/quest/m2/mesh-device-bounds.md) - the other mesh contract change before the cut
