# [M] Backport the relay HTTP hardening and credit-starved withdrawal to release

## Goal

`release`'s relay serves its HTTP routes with main's response hardening and ops-listener checks, and withdraws a moq-transport advertisement a peer starves of send credit, with the published API unchanged. The fixes ship in the next patch release a human cuts.

## Plan

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run): hand-port rather than cherry-pick where `release` lacks main's structure, as [moq-dev/moq#7389](https://github.com/moq-dev/moq/pull/7389) did, with a regression test for each on `release`. Version bumps stay with a human: several backports combine into one patch release, so these PRs bump nothing.

Added 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run):

- **Relay HTTP hardening:** port [moq-dev/moq#7210](https://github.com/moq-dev/moq/pull/7210) (merged): `/fetch` serves bytes as `application/octet-stream`, `/fetch` and `/announced` send `nosniff` and `private, no-store` (refusals included), and the ops listener refuses any request with `Origin` and any `Host` or URI authority that isn't the accepted socket's destination. `release` has its own `rs/moq-relay/src/fetch.rs` and `internal.rs`; port the behavior and #7210's tests, not its layout.
- **Withdraw without credit:** port [moq-dev/moq#7414](https://github.com/moq-dev/moq/pull/7414) (merged), keeping `release`'s published API: a withdrawn PUBLISH_NAMESPACE or REQUEST_UPDATE that a peer starves of send credit is reset in both directions instead of pinning the announce loop. Port its zero- and partial-credit withdrawal tests for the drafts `release` speaks.

Decided 2026-11-04 (maintainer, /quest-unblock): one quest, one `release` PR per port, as [moq-dev/moq#7599](https://github.com/moq-dev/moq/pull/7599) did, with [moq-dev/moq#7596](https://github.com/moq-dev/moq/pull/7596) kept as the main-side checkpoint; not splitting the ports into two quests. The maintainer cuts the combined patch release once both land.

Shipped: the dot-segment and auth-redirect refusals landed on `release` in [moq-dev/moq#7599](https://github.com/moq-dev/moq/pull/7599) (2026-11-02) and published in `moq-auth` 0.2.3 and `moq-relay` 0.17.11 (2026-11-03). That also settles the redirect follow-up [moq-dev/moq#7298](https://github.com/moq-dev/moq/pull/7298) and [moq-dev/moq#7278](https://github.com/moq-dev/moq/pull/7278) suggested. [moq-dev/moq#7596](https://github.com/moq-dev/moq/pull/7596) holds this quest's claim.

Public API: none. Wire: none; as on main, a withdrawal resets a partially written request without waiting for peer credit.

## Related

- [Backport the SETUP-path and iroh-key fixes](/quest/m0/backport-security-fixes.md) - the earlier backport, which shipped alongside the dot-segment and redirect refusals
