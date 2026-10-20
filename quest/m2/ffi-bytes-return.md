# [M] Zero-copy byte returns

## Goal

A frame payload returned to a binding (`read_frame`, `recv_datagram`, decoded
media) reaches the host without a copy, through a Rust-owned buffer the host
releases, in every generator of [moq-dev/uniffi](/quest/m2/uniffi-fork.md).
Proven on the [binding benchmark](/bench/README.md).

## Plan

Decided with the maintainer on 2026-10-13: build it in the fork rather than
wait for upstream (mozilla/uniffi-rs#2847). Gated on the payload numbers:
start only if, after the bulk copy, the remaining return copy is a
measurable share of the read path. Otherwise delete this quest and record
why.

- The buffer's lifetime must be explicit in each language: a host that
  keeps a payload view after release reads freed memory. Prefer a
  host-owned type that frees on its finalizer or close, matching each
  wrapper's handle idiom.
- Every generator gets the change, including Go, Dart, C++, and C.

Public API: returned payload types may change per language. Wire: none.

## Required

- [Payload bytes](/quest/m2/ffi-bytes.md) - the numbers that justify it
