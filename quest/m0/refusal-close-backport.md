# [XS] Backport the acknowledged-close pin to release

## Goal

On `release`, a browser refused by the relay reliably sees `unauthorized`
(close code 2), never "Connection lost", because the release line pins a
`web-transport-moq` 1.3.x with the acknowledged-close fix.

## Plan

`release` pins `web-transport-moq` 1.3.5, which starts the 100 ms teardown
grace before the CLOSE capsule is acknowledged. `main` already carries the fix
in moq-tokio's `quic::web` (#5664). Once the 1.3.x fix ships, bump the
release-line pin and run `just test interop --all`, keeping the exact browser
refusal assertion. Follow the backport workflow.

Public API: none. Wire: none.

## Required

- [noq 1.3.x publication](/quest/m0/refusal-close-backport-upstream.md) - the release-line fix is available
