# [XS] moq-relay's auth-trust test survives load

## Goal

`http_client::tests::auth_trust_is_independent_from_mesh_verification` in
moq-relay stops timing out when the host is busy, fixed at its cause.

## Plan

Implementation is in [moq-dev/moq#7550](https://github.com/moq-dev/moq/pull/7550).
The fixture dropped successful leases without serving their background `end`
POSTs. Reusing an ephemeral port let one steal the next trial's single accept,
leaving its admission waiting forever. Forcing port reuse reproduced this on the
first run; the unchanged test also timed out on diagnostic stress iteration 28.

The fix shares one listener across all six trust cases and serves and awaits each
lease's end report before advancing. The original trust assertion remains, with
additional connect/end event and session-ID checks. No timeout changed.
All 1,000 diagnostic stress iterations and the full local `just ci origin/main`
passed (731 Rust tests). Validation uses the pinned Nix Rust tools with
`BASH_ENV` unset; the inherited mbx launcher hits its known argument-size failure
in the unchanged Clippy-gate fixture.

Remaining: run the required other-vendor local review, address any findings,
then remove this quest and its link and mark the PR ready. `quest review --base
origin/main` could not run because the Claude CLI reported its weekly limit.
Resume with `/quest-iterate` when that reviewer is available; no implementation,
API, or scope decision is outstanding.

Decided 2026-10-30 (maintainer, /quest-plan of the 2026-10-30 /quest-complete follow-ups): ✅ new flake child / drop.

Public API: none. Wire: none.
