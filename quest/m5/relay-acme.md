# [M] Relay ACME on instant-acme

## Goal

moq-relay's `acme` feature returns: it issues and renews certificates through
instant-acme on `rind-instant-acme`, with no rustls-acme, and its graph holds
no aws-lc-rs.

## Plan

Decided by the maintainer on 2026-10-26: rustls-acme hard-wires ring or
aws-lc-rs for account keys, CSRs, and thumbprints, and has no provider hook
([FlorianUekermann/rustls-acme#62](https://github.com/FlorianUekermann/rustls-acme/issues/62)),
so moq-relay moves to instant-acme 0.9, which takes its crypto through its own
`CryptoProvider`. instant-acme is only the ACME client, so moq-relay now owns:

- the renewal loop that rustls-acme ran,
- the TLS-ALPN-01 challenge resolver in its rustls config,
- key and CSR generation through `rind-rcgen`.

Its cache's file naming moves to rind. m1 removed the feature, its config, and
its cache because nobody used ACME yet (maintainer, 2026-11-03), so the cache
needs no compatibility with the rustls-acme layout. Test against a local ACME
server such as Pebble.

The adapter is planned when this starts (maintainer, 2026-11-09, in rind's
quest audit): ✅ this quest begins by planning `rind-instant-acme` in rind's
quest tree (rejected: re-plan the adapter in rind now, ahead of its consumer;
leave the stale wait). rind deleted the adapter in
[moq-dev/rind#510](https://github.com/moq-dev/rind/pull/510) because nothing
used it yet, and carries none until this does. The wait-only quest that
pointed at rind's deleted one is gone.

Public API: moq-relay regains the `acme` feature and an ACME config section.
Wire: none.

## Waiting

- instant-acme: publishes 0.9, which carries the pluggable `CryptoProvider`.
  Checked 2026-11-09: its latest is 0.8.5. Check with `cargo search
  instant-acme`.
