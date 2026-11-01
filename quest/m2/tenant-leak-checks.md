# [XS] The tenant matrix's HTTP checks assert exact refusals

## Goal

The relay tenant matrix's HTTP check asserts a definite outcome for each
cross-tenant request, so it cannot pass by timing out or by an unexpected
status: B listing A's root gets an exact refusal or B's own listing, and B
fetching A's track gets an exact refusal, with no 2-second timeout standing
in for "nothing served".

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7366](https://github.com/moq-dev/moq/pull/7366): m2, as
recommended. Start after [moq-dev/moq#7366](https://github.com/moq-dev/moq/pull/7366) (open) lands; it adds
`rs/moq-relay/tests/integration/tenant_matrix.rs`.

Its `http_routes_keep_tenants_apart` accepts any status for
`/announced/tenant-a?jwt=b` (checking the body only when it is 200) and
treats a `/fetch` still waiting after 2 s as "served nothing", since the
handler waits up to a hard-coded 30 s (`rs/moq-relay/src/web.rs:1096`).

- Pin the expected status of each request (what the auth contract says B
  gets for a path outside its root) and assert it.
- Make the fetch answer definitively: either the refusal comes back before
  the wait (the path is outside B's root, so no wait is needed), or the wait
  becomes configurable so the test can set it short. Pick the one that
  needs no new knob if possible.

Public API: none, unless the fetch wait becomes a config field. Wire: none.

## Related

- [Tenant matrix](/quest/m2/tenant-matrix.md) - [moq-dev/moq#7366](https://github.com/moq-dev/moq/pull/7366) lands the test this tightens
