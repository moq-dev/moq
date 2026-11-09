# [S] An accept whose client already left fails instead of hanging

## Goal

A server's `request.accept()` that starts after its client has already shut
down fails promptly with a closed error, instead of waiting forever. This
holds in Rust and in every binding, and a regression test covers it. A peer
that connects and drops before the server accepts must not pin a server task.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete follow-up of
[moq-dev/moq#8363](https://github.com/moq-dev/moq/pull/8363)). ✅ m1
(rejected: m0, m3, dropping it).

While writing #8363's tests, an early version of its Python test saw the
hang: the client exited, then the server's `request.accept()` started and
never returned. #8363 changed the test rather than the server, since the
hang sits in the server handshake path, outside its goal.

Reproduce it first, through the Python bindings as seen and directly in
moq-ffi and moq-tokio, to find which layer waits on a session that is
already gone. Fix it at that root cause, not with a timeout. If it reproduces
on `release` and a peer can trigger it, suggest a backport.

Public API: none expected. Wire: none.
