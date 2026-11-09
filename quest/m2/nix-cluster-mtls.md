# [M] Nix clusters authenticate leaves with mTLS

## Goal

A clustered `services.moq-relay` deployment authenticates its leaves with
client certificates from a cluster CA instead of a JWT. Each leaf presents a
`connect.tls` certificate when it dials the root; the root verifies it under
`listen.tls.root`, and its `moq auth serve` grants it as a peer through
`--mtls-link peer` (documented in `doc/bin/relay/auth.md` § mTLS). The module
no longer mints a cluster token, so no unbounded credential is generated, and a
leaked leaf credential is revoked by rotating or scoping its certificate.

## Plan

Decided 2026-11-08 (maintainer, /quest-complete of
[moq-dev/moq#8205](https://github.com/moq-dev/moq/pull/8205)): ✅ mTLS for Nix
cluster auth (rejected: mesh tokens, the other refreshable leaf credential the
PR proposed). The same decision
made token lifetime a signing policy only, so `moq auth serve` admits a token
without `exp` and the module's `moq auth sign --unbounded` cluster token keeps
working until this quest replaces it.

- `nix/modules/moq-relay.nix`: add options for the cluster CA (the root's
  `MOQ_LISTEN_TLS_ROOT`) and each leaf's client certificate and key (its
  `MOQ_CONNECT_TLS_CERT` / `MOQ_CONNECT_TLS_KEY`, checking the relay's config
  for the exact keys). Decide whether the module generates a CA and leaf
  certificates for a single-host test setup, or only takes paths; a CA key
  generated on one host cannot reach the others, so paths are the likely
  default.
- Drop the leaf's `moq auth sign` preStart and the generated
  `cluster.jwt`. Keep or remove `cluster.tokenFile` for deployments that bring
  their own token; removing it is a break of the module's options and needs a
  migration note.
- The root's `moq auth serve` already runs `--mtls-publish '**'
  --mtls-subscribe '**' --mtls-link peer`; check that a leaf's certificate
  reaches it and that the root refuses a leaf without one.
- `doc/bin/relay/cluster.md`: show the Nix cluster with certificates.
- Verify by evaluating the module for a root and a leaf; the
  [peer-list eval check](/quest/m4/nix-relay-peers-test.md) is the place for a
  lasting test if it lands first.

Public API: Nix module options only. Wire: none.
