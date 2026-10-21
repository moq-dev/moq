# [M] @moq/qmux lives in the monorepo

## Goal

The TypeScript qmux (`@moq/qmux`, the WebSocket fallback Safari and every
browser without WebTransport uses) is built, tested and published from
`js/` in this repo. `js/net` and `js/p2p` depend on the workspace package,
and nothing in `js/` depends on moq-dev/web-transport.

## Plan

Decided 2026-10-20 (maintainer): moq's transports come from this repo,
with `moq-quic` powering qmux and WebTransport. web-transport stays
published but moq takes no dependency on it, except possibly
web-transport-proto.

- Import the package with its history if cheap, otherwise as a fresh copy
  at the version `js/net` pins (`^0.3.3`), and publish the next version from
  here under the same npm name so external users keep upgrading.
- `@moq/web-socket-stream`: fold it into `@moq/qmux` unless something
  outside qmux needs it on its own (maintainer: no strong view). Folding
  ends that package for any direct consumer, a breaking change: deprecate
  it on npm with a pointer to `@moq/qmux` and add a `## Unreleased` entry to
  `@moq/qmux`'s `MIGRATE.md`. No re-export shim.
- Keep all four wire versions the Rust qmux negotiates. The Rust
  [qmux on moq-quic](/quest/m1/quic-qmux.md) interop tests then run against
  this in-tree peer instead of a published copy.
- Wire it into `just check`, CI and the release tooling like the other
  `js/` packages.

Public API: `@moq/qmux` keeps its name and exports; folding
`@moq/web-socket-stream` in removes that package (breaking, see above).
Wire: none.

## Related

- [qmux on the QUIC stream state machine](/quest/m1/quic-qmux.md) - the Rust side of the same move
