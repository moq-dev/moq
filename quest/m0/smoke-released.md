# [S] Resolve the remaining external Smoke interoperability failures

## Goal

The Smoke workflow passes on main, with every red cell fixed at its cause.
Smoke clients continue to use the interfaces their installed artifacts expose.

## Plan

The bounded client fixes are prepared in
[moq-dev/moq#6981](https://github.com/moq-dev/moq/pull/6981): Go uses the
published `Route`, C uses the published status callback and frame reads,
and Cloudflare's current Datagram gets its status and end-of-group fields.
The release-cut guidance now keeps smoke clients aligned with published APIs.
`freshness.sh` still enforces Go `@latest` and the newest `libmoq`.

The old Nix workspace-member failure is resolved by merged
[moq-dev/moq#6931](https://github.com/moq-dev/moq/pull/6931).
The latest [Smoke run](https://github.com/moq-dev/moq/actions/runs/40608138308)
now builds the Docker relay successfully, then fails actual external
interoperability: the second runner matrix passes 6 of 20 cells and fails 14.
Read its uploaded reports and establish each remaining cause before changing
any protocol behavior. This is no longer the missing-workspace-target fix.

After the client PR lands, inspect the next normal Smoke run for the Go, C,
and Cloudflare lanes, and investigate remaining red cells at their source.
Do not weaken a lane or retry media exchange to make it pass. No release,
Nightly, or Smoke dispatch was needed for the client fixes.

Public API: none. Wire: none.
