# [S] Mesh enrollment is bounded and revocation takes effect

## Goal

The mesh coordinator (`rs/moq-auth/src/mesh/coordinator.rs`) stops assuming its device store stays small and its revocations wait for token expiry:

- A reusable enrollment key carries a use budget set when it is created, so the coordinator holds at most what its keys allow. Today a reusable key enrolls any number of endpoints, and each enroll clones the whole store, rewrites the state file and returns every member under one mutex, so a leaked key can mint thousands.
- A revoked device loses access within a bounded time, not only at its token's `exp` (1h by default): either `decide` checks revocation for mesh tokens, or mesh tokens are short-lived with revalidation on.

## Plan

Found by the 2026-11-07 invariant audit (auth-7, auth-8). The budget belongs to the key's owner, not a server-wide constant or rate limit, per REVIEW.md's rule against constant caps.
