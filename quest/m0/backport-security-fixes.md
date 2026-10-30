# [S] Backport the SETUP-path and iroh-key fixes to release

## Goal

`release` refuses a moq-transport SETUP Path on WebTransport and WebSocket
bindings, as [moq-dev/moq#7206](https://github.com/moq-dev/moq/pull/7206)
fixed on main, and writes persisted iroh endpoint keys with private file
permissions, as [moq-dev/moq#7213](https://github.com/moq-dev/moq/pull/7213)
did. Both ship in one patch release with the published API unchanged.

## Plan

Decided 2026-10-30 (maintainer, `/quest-plan`): one quest and one PR against
`release` for both. They are small security fixes that ship in the same patch
release, so pairing them is simpler than the usual one PR per backport.

- SETUP path: `release`'s handshake also accepts a SETUP Path that names a
  different path or query than the request URI, so Tokio could authorize the
  wrong path. Port #7206's refusal onto `release`'s handshake, with its test.
- iroh key: port #7213's private permissions for the persisted key file, with
  its test.
- Version bumps are hand-picked, per `CONTRIBUTING.md`.

Public API: none. Wire: a conflicting SETUP Path is refused, as on main.
