# [M] Cluster discovery flags

## Goal

A cluster discovery mechanism carries what it needs, so a relay cannot be
configured with a mechanism that is missing its prerequisite. Today three
startup checks exist only because the flags let you say something incomplete.

## Plan

The four mechanisms compose rather than compete (`can_dial` is outbound peers
or LAN, `has_work` is that or gossip), so this is not about making them
exclusive. It is about the `requires` edges between a mechanism and the flags
it cannot work without:

- `--cluster-mesh` bails without `--cluster-node`, because gossip has no
  address to advertise.
- `--cluster-lan` bails without `--cluster-node` for the same reason.
- `--cluster-lan` bails without `--cluster-lan-secret`, because mDNS is
  unauthenticated and any advertiser on the network would otherwise be dialed
  and handed `--cluster-token`.

Each is a real rule with a good error message, and each exists because the
flag shape allows a state that has no meaning. Make the mechanism's value
carry its prerequisite instead: mesh and LAN both need this relay's own URL,
and LAN needs its secret, so those belong to the mechanism rather than sitting
beside it as an independent flag another mechanism might or might not want. A
compile error beats a runtime check, and an operator finding out at startup
that a flag needed a companion is the failure mode this removes.

The remaining `ensure!`s stay. An unattached QUIC client or missing client TLS
is API misuse by an embedder, not something an operator can express wrongly,
so they belong where they are.

Every rename is user-facing, so old spellings stay as hidden aliases per the
deprecation rules: no `--help` entry, no "deprecated, use X" note, and `/doc`
examples updated to the new names only. The flags are TOML keys too, so this
is a config-file migration as well as a CLI one, and `--cluster-node` is read
by `moq-relay` alone. Grep the binary name repo-wide and reconcile every
sample invocation under `doc/bin/`, `doc/setup/`, and `demo/` against
`--help`.

Leave `--cluster-linger` alone: it is already a hidden no-op.
