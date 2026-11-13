# [S] Challenge proof for certificate credentials

## Goal

A relay can ask a client presenting a [certificate credential](/quest/m3/mesh/cert-credentials.md)
to sign a fresh nonce instead of a timestamp, so a credential replayed within
the time window admits nothing. Clients that support it answer the
challenge; a relay configured to require it refuses time-window proofs.

## Plan

Decided 2026-11-07 (maintainer, Kixel's `/quest-plan`: "plan the challenge,
but do the simple thing first"): certificate credentials ship with a
time-window proof, and this closes its replay window later. Settle where the
nonce rides (SETUP parameters on moq-transport and moq-lite, and what a
WebSocket or HTTP admission does) and what one extra round trip costs a
connect, measured, before coding.

Tests: a replayed time-window proof is refused when the relay requires the
challenge; a client signs the nonce and is admitted; a mismatched nonce is
refused.

Public API: a challenge option on the relay and the clients. Wire: a nonce
exchanged during setup.

## Required

- [Certificate credentials](/quest/m3/mesh/cert-credentials.md) - the credential this hardens
