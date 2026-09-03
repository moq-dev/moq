# [M] Choose the parent and establish the QUIC fork

## Goal

Choose Quinn or noq as the parent, create the maintained moq-dev fork, and
record who owns rebases, releases, security updates, and upstream
coordination. Every later quest can name one concrete protocol core and one
package source.

## Plan

Start with Quinn. Select noq only when both of these gates pass:

- the n0/Iroh maintainers explicitly welcome the hierarchical scheduler and
  qmux state-machine work as upstream contributions;
- porting the green [Quinn qmux prototype](https://github.com/kixelated/quinn/pull/2)
  plus a minimal scheduler-key spike does not require coupling either feature
  to noq's multipath path state.

Audit the two candidates against the needs already exercised here: sans-IO
driving from `moq-uring`, rustls, connection-ID routing, pacing and transmit
batching, stream priority updates, reliable-reset stream-state changes,
per-stream delivery accounting, qlog, and the ordinary `moq-native` async
adapters. This is a code and ownership audit, not a new performance bakeoff.
Both candidates get the same relay benchmark gate when adopted.

The evidence at planning time is mixed but useful. noq is a Quinn-derived
stack maintained for Iroh, and n0 merged
[noq#667](https://github.com/n0-computer/noq/pull/667), a port of
[Quinn#2601](https://github.com/quinn-rs/quinn/pull/2601), while the Quinn PR
remains open. Quinn has the smaller delta from MoQ's current default and is
already the home of the qmux prototype. Record any maintainer response and the
port diff in this quest before making the choice.

Create the fork only after choosing. Protect its main branch, mirror the
parent's security advisories, define a regular upstream-sync check, and keep
each carried feature as a separable commit series. Decide package identity at
the same time: changes not released by the parent need uniquely named
moq-dev packages before a published MoQ crate can consume them.
