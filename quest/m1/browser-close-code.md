# [M] A browser sees the close code on every WebTransport backend

## Goal

A browser interop case proves a relay's rejection reaches the page: the relay
closes the session with an application code and reason, and the page reads
both from `WebTransport.closed`. Every `moq-dev/web-transport` backend that
speaks WebTransport (noq, quinn, iroh) delivers the close capsule, verified by
that case or an equivalent test, and any that doesn't is fixed upstream.

## Plan

[moq-dev/noq#23](https://github.com/moq-dev/noq/pull/23) (backported in
[#24](https://github.com/moq-dev/noq/pull/24)) fixed `web-transport-moq`'s
close path with a Rust regression test, but only
Chromium treats the H3 control stream ending early as fatal, so no Rust peer
proves the browser path. The playwright interop harness in `test/interop/`
has no close-code scenario yet.

- Add the browser case: a relay (or `Request::reject`) closes with a code, and
  the page asserts the code and reason. It should fail with the capsule fix
  reverted.
- Audit `web-transport-noq`, `-quinn`, and `-iroh` for the same bug: a close
  capsule sent from a task that doesn't keep the H3 control streams alive.
  `web-transport-iroh` 0.7 sends no capsule at all, only a QUIC close; decide
  there whether iroh's WebTransport path should send one.
- Fix upstream, release, and bump the pins.

Decided with the maintainer: the browser case and the backend audit ship
together, since the case is what proves each backend.

Public API: none. Wire: none, unless iroh starts sending the capsule.
