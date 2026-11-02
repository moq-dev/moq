# [XS] moq-relay's auth-trust test survives load

## Goal

`http_client::tests::auth_trust_is_independent_from_mesh_verification` in
moq-relay stops timing out when the host is busy, fixed at its cause.

## Plan

Seen during [moq-dev/moq#6994](https://github.com/moq-dev/moq/pull/6994)'s
local `just ci`: it timed out under load and passes alone in 0.08 s. Find what
it waits on by wall clock and make it deterministic, rather than raising a
timeout.

Decided 2026-10-30 (maintainer, /quest-plan of the 2026-10-30 /quest-complete follow-ups): ✅ new flake child / drop.

Public API: none. Wire: none.
