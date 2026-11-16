# [S] The relay's config names agree with each other at the freeze

## Goal

Four names in moq-relay's configuration follow the rules the rest of it
already keeps. The QUIC-LB flags join their section and key
(`--listen-lb-*` for `[listen.lb]`). The peer-list endpoint's CA is
`cluster.api.root`, as the auth endpoint's is `auth.root`. The NixOS module's
`cluster.rootUrl` is `cluster.connect`. `cluster.max_hops` says it bounds
only the older wire versions. Each old spelling is refused with the new one
named.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ `--listen-quic-lb-*` becomes `--listen-lb-*` (rejected: renaming the
  table to `[listen.quic_lb]`; documenting a third exception). The docs'
  rule is that a flag joins the section and key, and the table is the
  shorter name.
- ✅ `cluster.api.tls.root` becomes `cluster.api.root` (rejected: keeping
  the nested table). Both are "the CA for an HTTP endpoint the relay
  calls", and a one-key `tls` table nests nothing.
- ✅ The nix option `cluster.rootUrl` becomes `cluster.connect` (rejected:
  keeping a name from the root and leaf mode, which
  [moq-dev/moq#8854](https://github.com/moq-dev/moq/pull/8854) removed).
- ✅ `cluster.max_hops` is marked legacy (rejected: deleting it now, which
  leaves lite-03 to lite-06 meshes without their bound while the relay
  still negotiates them).

Facts found by the audit:

- `doc/bin/relay/config.md:8-11`: "Every key is also a CLI flag and
  environment variable that joins the section and key", with `listen.bind`
  and the two `extensions` keys as the listed exceptions. `[listen.lb]`
  (`rs/moq-tokio/src/listen.rs:204`) is spelled `--listen-quic-lb-id`,
  `-nonce`, and `-forward-bind`, `-key`, `-peers` (`:387-462`), an unlisted
  third. `release` has `--listen-quic-lb-id` and `-nonce`; the three
  `forward` flags are new. `--server-quic-lb-*` is already refused naming
  `--listen-quic-lb-*` (`:387-396`, `:664`).
- `auth.root` (`rs/moq-relay/src/auth.rs:46`) is flat;
  `cluster.api.tls.root` (`rs/moq-relay/src/cluster/api.rs:38-46`) is new
  since release and is the only key in its table.
- `nix/modules/moq-relay.nix:110` declares `cluster.rootUrl`, which sets
  `MOQ_CLUSTER_CONNECT` (`:376-378`) and is named in two other options'
  docs and an assertion (`:144`, `:162`, `:232-237`). `release` has the
  option, setting `MOQ_CLUSTER_ROOT`.
- `cluster.max_hops` (`rs/moq-relay/src/cluster.rs:755`) bounds a hop list
  that only lite-03 to lite-06 and the moq-cluster extension carry; a
  lite-07 route has no hop list to bound. Nothing in its doc says so.
- `rs/moq-relay/src/cluster.rs:728`: "Hop-based routing on broadcasts
  prevents announcement loops regardless of topology." Stale since lite-07.

For the PR:

- Flags and environment variables: `--listen-lb-id`, `--listen-lb-nonce`,
  `--listen-lb-forward-bind`, `-key`, `-peers`, with `MOQ_LISTEN_LB_*`.
  Refuse `--listen-quic-lb-*` and `--server-quic-lb-*` (and their
  environment variables) through `moq_tokio::cli::Deprecated`, naming the
  new spelling, as `--server-quic-lb-*` is refused today. The TOML keys do
  not move.
- `cluster.api.root`, `--cluster-api-root`, `MOQ_CLUSTER_API_ROOT`. The old
  key was never released, so it needs no refusal; an unknown key already
  stops startup.
- Nix: rename the option and its mentions. Recommended:
  `lib.mkRemovedOptionModule` naming `cluster.connect`, since the option is
  on `release` and the repository refuses old names rather than aliasing
  them. No other module in this repository sets it; downstream NixOS
  configs do, so the PR's Impact section names the rename.
- `cluster.max_hops`: its doc, `doc/bin/relay/config.md`, and
  `doc/bin/relay/cluster.md` say it applies to moq-lite-03 through 06 (and
  moq-transport's cluster extension while
  [the switch](/quest/m2/multicast/switch.md) keeps it) and leaves with
  lite-06. `origin::Config::max_hops` gets the same sentence. If the switch
  retires the cluster extension first, drop that clause.
- Fix the struct doc at `cluster.rs:728`.
- Update `doc/bin/relay/config.md`, `doc/bin/relay/cluster.md`,
  the migration notes for moq-relay and moq-tokio, and
  `rs/moq-relay/tests/integration/released_cli.rs`, the frozen list of
  released spellings that must keep parsing: it gains
  `listen-quic-lb-id` and `listen-quic-lb-nonce`.
- Tests: each old flag and variable refuses with the new name in the
  message; the new ones parse; `nix flake check` covers the module.

Not picked up, recorded so the next audit does not reopen them without new
evidence: `web.https.cert` not falling back to `listen.tls.cert` (additive
later), and `listen` against `bind` for socket keys (already on release).

Public API: `moq_tokio::listen` flag names only. Config and CLI: breaking,
five flags and their variables renamed (two released), one unreleased TOML
key renamed, one nix option renamed. Wire: none.

## Related

- [The switch](/quest/m2/multicast/switch.md) - retires what else reads a hop bound
