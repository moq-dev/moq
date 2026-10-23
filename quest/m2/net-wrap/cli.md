# [S] CLI paths take the traits

## Goal

`moq-cli`'s publish and play paths hand moq-mux a broadcast through the
[moq-net traits](/quest/m2/net-wrap/traits.md), so the
[encrypted CLI](/quest/m2/e2ee/cli.md) only swaps in an encrypted broadcast
and adds no crypto to the import, capture, export, or playback code.

## Plan

- Make the code between the session and moq-mux generic where it holds a
  concrete moq-net broadcast or track: import, capture, export, and play.
- Paths that inspect a broadcast's content for the operator, such as stats
  and echo, stay concrete; the encrypted CLI refuses them.
- No behavior change: the existing CLI tests pass unmodified.

Public API: none outside moq-cli. Wire: none.

## Required

- [moq-mux takes the traits](/quest/m2/net-wrap/mux.md) - the CLI drives moq-mux
