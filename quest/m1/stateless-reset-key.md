# [S] A restarted relay resets its predecessor's connections

## Goal

A relay restarted on the same address with the same configured secret
stateless-resets the connections its predecessor held, so a publisher learns
within one RTT of the relay coming back and a subscriber within one
keep-alive interval, instead of waiting out the QUIC idle timeout. A relay
without the secret keeps today's random per-process key.

## Plan

Decided 2026-10-19 by the maintainer (#5861): land
[#5910](https://github.com/moq-dev/moq/pull/5910), which already builds it.

- The secret is a file named by `[listen] reset_key`
  (`--listen-quic-reset-key`, `MOQ_LISTEN_QUIC_RESET_KEY`), not derived from
  the TLS key: ACME and certificate rotation would change a derived key
  silently.
- HKDF derives both the reset key and the hashed connection ID key from it.
  An unsharded, non-LB tokio server hashes connection IDs with a per-process
  key too, so a stable reset key alone would drop the old IDs as invalid
  before resetting them.
- Cover tokio and io_uring, with the crash-and-restart test #5910 carries.
- #5910 rebases onto [#5878](https://github.com/moq-dev/moq/pull/5878)
  (receive leases).
- Don't reuse `cluster.token` or `cluster.lan.secret`: one is a credential,
  the other keys the LAN mesh.

Public API: a new relay config field and `moq_quic::EndpointConfig::reset_secret`.
Docs: `doc/bin/relay/`.

## Closes

- [#5861](https://github.com/moq-dev/moq/issues/5861) - a restarted relay can't stateless-reset its predecessor's connections
