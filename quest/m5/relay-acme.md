# [M] Relay ACME on instant-acme

## Goal

moq-relay's `acme` feature returns: it issues and renews certificates through
instant-acme on a rind `CryptoProvider` adapter, with no rustls-acme, and its
graph holds no aws-lc-rs.

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

Decided 2026-11-09 (maintainer, /quest-unblock): rind deleted
`rind-instant-acme` on 2026-11-08
([kixel-ai/rind#510](https://github.com/kixel-ai/rind/pull/510)) because no
consumer used it, so this quest waits on instant-acme 0.9 directly instead of
on that crate. Restore the adapter in rind from #510's parent commit, published
this time (rejected: dropping relay ACME, and leaving the wait on the deleted
crate).

Public API: moq-relay regains the `acme` feature and an ACME config section.
Wire: none.

## Waiting

- djc/instant-acme: a 0.9 release ships the pluggable `CryptoProvider`
  ([djc/instant-acme#268](https://github.com/djc/instant-acme/pull/268),
  merged 2026-03-30). Check `cargo info instant-acme`; it was 0.8.5 on
  2026-11-09.
