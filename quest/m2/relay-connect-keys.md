# [S] The relay refuses the two dial keys it has no use for

## Goal

`moq-relay` stops at startup when given `connect.url` (`--connect`,
`MOQ_CONNECT`) or `connect.tokens` (`--connect-token`,
`MOQ_CONNECT_TOKEN`), with a message naming `cluster.connect` and
`cluster.token`. A relay's cluster dials carry only the credentials the
cluster settings name, so no token reaches a peer the operator did not
choose it for, a LAN peer included. The other `[connect]` keys (bind, TLS,
versions, timeouts) keep working as the template for cluster dials.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Refuse both keys by name (rejected: splitting the `[connect]` fragment
  so the relay flattens only the dial settings, a larger change to a type
  the clients share; documenting them as they behave).
- ✅ Reproduce the token reaching a cluster dial before changing anything:
  the audit found it by reading, ran nothing, and no test covers it.

Facts found by the audit:

- The relay flattens the whole client fragment
  (`rs/moq-relay/src/config.rs:42`, `pub connect:
  moq_tokio::connect::Config`).
- `connect.url` (`rs/moq-tokio/src/connect.rs:513-514`) is read only by
  `Client::publish` and `Client::consume` (`rs/moq-tokio/src/client.rs:164`,
  `:346`, `:356`). Nothing in `rs/moq-relay/src` reads it, so
  `moq-relay --connect https://peer` parses and does nothing, where the
  operator meant `--cluster-connect`. The relay's rule elsewhere is that a
  setting nothing reads stops startup.
- `connect.tokens` (`rs/moq-tokio/src/connect.rs:524-530`, new since
  release) is documented as "credentials to present on every dial".
  `peer_client` (`rs/moq-relay/src/cluster.rs:2316-2345`) clones the whole
  template for each cluster dial, `rs/moq-tokio/src/client.rs:165` keeps its
  tokens, and `rs/moq-tokio/src/connection.rs:952` presents them. The LAN
  branch (`cluster.rs:2330`) narrows versions and pins a fingerprint but
  leaves the tokens, while `doc/bin/relay/cluster.md:250-251` says a LAN
  peer "is never handed `cluster.token`". So `connect.tokens` is an
  undocumented second cluster credential that goes to every peer.

For the PR:

1. Write the failing test first: a relay configured with `connect.tokens`
   and a static peer, then a LAN peer, and assert what each peer's SETUP and
   AUTH streams receive. If no token arrives, the leak is not real: say so
   in the PR, and the refusal still lands for the unread key.
2. Refuse both in `Config::resolve` (`rs/moq-relay/src/config.rs:322`),
   beside the released `[client]` table it already names `[connect]` for.
   The message names the flag, the environment variable, and the TOML key in
   use, and what replaces each: `cluster.connect` for the URL, and
   `cluster.token` or a peer's `token` for the credential.
3. Clear the template's tokens in `peer_client` as well, so an embedder that
   builds a `Cluster` with `with_connect` gets the same guarantee as the
   binary.
4. `moq-relay --help` and `doc/bin/relay/config.md` stop listing the two
   keys under `[connect]`; `doc/bin/relay/cluster.md` keeps its sentence
   about LAN peers, now true by construction.

Tests: the refusal names each spelling; the reproduction from step 1 passes
with no token on any cluster dial; a relay with the remaining `[connect]`
keys still dials its peers.

Public API: none (`Cluster::with_connect` keeps its type; its tokens are
ignored). Config and CLI: breaking for moq-relay only, `connect.url` and
`connect.tokens` refused. Wire: none.

## Related

- [The relay's config names](/quest/m2/relay-config-names.md) - the renames at this freeze
