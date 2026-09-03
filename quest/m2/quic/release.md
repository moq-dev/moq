# [M] Release the custom QUIC stack

## Goal

Immutable releases of the selected protocol core, async adapter, WebTransport
adapter, and qmux implementation are available to every published MoQ crate.
The workspace lockfile identifies exact released sources, with no root-only
Cargo patch or mutable git branch.

## Plan

Prefer parent releases for every accepted upstream patch. For carried fork
changes, publish uniquely named moq-dev packages and make package renaming
explicit in Cargo manifests so downstream consumers resolve the same core.
Do not publish a crate that silently impersonates `quinn`, `quinn-proto`,
`noq`, or `noq-proto`.

Release the dependency chain from the bottom up, including the matching
`web-transport-trait` scheduling surface and qmux adapter. Pin each released
version in this repository's workspace dependencies and regenerate
`Cargo.lock`. Verify minimal/default/all feature builds so enabling Quinn,
noq, qmux, or the uring runtime cannot unify two incompatible copies of the
protocol state.

Document the parent commit, carried patches, upstream PRs, and security-update
procedure in the fork release. A release is incomplete if consumers cannot
tell whether an advisory against the parent applies.

## Required

- [Choose the parent and establish the fork](/quest/m2/quic/parent.md) - owns
  repository and package identity
- [Land the Quinn maintenance backlog](/quest/m2/quic/quinn-maintenance.md) -
  avoid carrying already-reviewed fixes as unexplained private patches
- [Reliable stream reset](/quest/m2/quic/reliable-reset.md) - provides the
  WebTransport-required transport extension
- [Hierarchical stream scheduling](/quest/m2/quic/scheduler.md) - provides the
  new transport API
- [qmux on the QUIC stream state machine](/quest/m2/quic/qmux.md) - provides
  the shared qmux implementation
