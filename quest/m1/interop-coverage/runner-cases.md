# [M] Our runner clients run the subgroup and namespace cases

## Goal

The moq-dev-rs and moq-dev-js clients in
[englishm/moq-interop-runner](https://github.com/englishm/moq-interop-runner)
implement `data-subgroup-basic` and `subscribe-namespace-lifecycle`
(moq-dev-js lacks both, moq-dev-rs lacks the first), and both pass against
moq-relay.

## Plan

Decided with the maintainer on 2026-10-21. These cases exist in the
runner's `docs/TEST-SPECIFICATIONS.md`, so implement them in our clients
there rather than in-tree; the [interop audit](/quest/m1/audit-interop.md)'s
standing approval covers PRs touching the `moq-dev-*` builds.

As of the runner report of 2026-10-19, moq-relay fails
`data-subgroup-basic` from third-party clients: objects carry one extension
where zero were sent, group 0 is missing, and END_OF_TRACK arrives where
END_OF_GROUP was expected. moq-relay passing that case is part of this
quest. `subscribe-namespace-lifecycle` from our Rust client times out
against moxygen, quic-zig, and Cloudflare draft-14, likely the draft-14
empty-prefix bug the interop audit owns.

Public API: none. Wire: none.
