# [S] Backport the dot-segment and auth-redirect refusals to release

## Goal

The `release` branch's relay refuses connection paths with `.` or `..` segments before asking the auth server, and `release`'s auth client refuses redirects from the auth server, matching main. The fixes ship in the next patch release a human cuts.

## Plan

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run).

- **Dot segments:** [moq-dev/moq#7366](https://github.com/moq-dev/moq/pull/7366) makes `Auth::admit` in `rs/moq-relay/src/auth.rs` refuse any path segment that is `.` or `..`, percent-encoded or not. Before it, a raw-QUIC SETUP path like `/tenant-a/../tenant-b?jwt=b` reached the auth server unresolved. Port it once #7366 merges.
- **Auth redirects:** on `release`, `rs/moq-auth/src/client.rs:35` builds the client without a redirect policy, so reqwest follows up to 10 redirects and re-sends the credential's JSON body on a 307/308. Main refuses since [moq-dev/moq#6384](https://github.com/moq-dev/moq/pull/6384) (`redirect::Policy::none()`, plus an error naming the redirect). Found by the agent on [moq-dev/moq#7298](https://github.com/moq-dev/moq/pull/7298), which fixes the cluster client but not this one.
- Hand-port rather than cherry-pick where `release` lacks main's structure, as [moq-dev/moq#7389](https://github.com/moq-dev/moq/pull/7389) did, with a regression test for each on `release`.
- Version bumps stay with a human: several backports combine into one patch release, so this PR bumps nothing.

Public API: none. Wire: none.

## Required

- [Tenant matrix](/quest/m2/tenant-matrix.md) - its PR, #7366, lands the dot-segment refusal on main

## Related

- [Backport the SETUP-path and iroh-key fixes](/quest/m0/backport-security-fixes.md) - the earlier backport in the same patch release
