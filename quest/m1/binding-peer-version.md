# [M] Binding peer version

## Goal

A session opened through any binding advertises that binding and its version
ahead of the Rust core, so an auth service can tell a Python client from a
Swift one: for example `moq-py/0.3.0 moq-ffi/0.4.12 moq-net-rs/0.3.10`.

Covers the py, swift, kt, go and dart wrappers, moq-c, cpp/moq, and the OBS
plugin (cpp/obs). Wire: none beyond the core quest. Public API: one
additive moq-ffi config field.

## Plan

Decided with the maintainer on 2026-10-22, alongside
[Peers advertise their implementation and version](/quest/m1/peer-version.md):

- moq-ffi's client and server config gain an `implementation` prefix: the
  caller's own product tokens, which moq-ffi joins ahead of its default
  `moq-ffi/<v> moq-net-rs/<v>`. A prefix rather than a full override keeps
  the core tokens truthful for rollout counts, and every caller here only
  needs to prepend.
- Each hand-written wrapper sets it from its own package version, read at
  build time where the language allows. moq-c exposes the field so cpp/moq
  and the OBS plugin can set theirs; the OBS plugin reports itself
  (`moq-obs/<v>`) ahead of `moq-cpp`.
- The generated `go/ffi` and `dart/moq_ffi` layers regenerate; the
  hand-written `go/wrapper` and `dart/moq` set the value.
- Update `doc/lib/{py,swift,kt,go,dart,c,cpp}` and `doc/bin/obs.md` only
  where they list connect options.

Tests: each binding's existing connect test asserts the advertised value
starts with its own token, read back through the relay or a local server's
peer accessor.

## Required

- [Peers advertise their implementation and version](/quest/m1/peer-version.md) - the SETUP parameter, the moq-net field, and moq-ffi's default token
