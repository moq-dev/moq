# [XS] The tenant matrix's listing check asserts an exact status

## Goal

The relay tenant matrix's HTTP check asserts a definite outcome when B lists
A's root: an exact refusal status or B's own listing, never whichever status
comes back.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7366](https://github.com/moq-dev/moq/pull/7366), which added
`rs/moq-relay/tests/integration/tenant_matrix.rs`.

`http_routes_keep_tenants_apart` accepts any status for
`/announced/tenant-a?jwt=b`, checking only that the body omits A's broadcast
and, when the status is 200, that it lists B's probe
(`tenant_matrix.rs:543-549` at 4bf501b535). Pin the status the auth contract
gives B for a path outside its root, and assert it.

The fetch half is done:
[moq-dev/moq#8087](https://github.com/moq-dev/moq/pull/8087) publishes a
decoy at B's root and asserts B's `/fetch` of A's path returns 200 with the
decoy, so it no longer leans on a timeout.

Public API: none. Wire: none.
