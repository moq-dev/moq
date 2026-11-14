# [S] The send-stream trait has its acknowledgment hook

## Goal

`moq_net::transport::poll::SendStream` gains the shape the acknowledgment
hook needs: an associated `Offset` type, `offset()`, and `poll_acked` with a
default that reports the hook as unsupported. Every transport in the tree
compiles against it, and none has to implement the hook yet. The trait is
public and unsealed, so adding these after the cut would break every
implementer.

## Plan

Decided 2026-11-12 (maintainer, /quest-audit before the cut): ✅ land the
trait shape in m2 and leave the drivers to the
[acknowledgment hook](/quest/m3/quic-ack-hook.md) (rejected: an extension
trait after the cut, which puts a new bound on moq-net's generic session
code and is itself a break; accepting another moq-net break later).

- Take the shape from the hook quest's Plan; the tokio driver already has an
  `Offset` (`rs/moq-tokio/src/quic/driver/send.rs`, from
  [moq-dev/moq#6950](https://github.com/moq-dev/moq/pull/6950)), and
  moq-quic's `poll_acked` exists behind `#[cfg(test)]`.
- "Unsupported" is a typed answer a caller can branch on at setup, not an
  error it must match at runtime: a session decides once whether its
  transport reports acknowledgments.
- JS has no counterpart: WebTransport exposes no acknowledgment, so `js/net`
  is unchanged; say so in the PR.

Public API: breaking for transports implementing `poll::SendStream` outside
the tree only if a method lacks a default; aim for none. Wire: none.
