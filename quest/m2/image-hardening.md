# [XS] The relay image runs unprivileged and builds reproducibly

## Goal

The published container image runs as a non-root user, and its builder stage is pinned so the same source produces the same image.

Today the final stage is `FROM scratch` with no `USER`, so the relay runs as uid 0, and the builder is `docker.io/nixos/nix:latest`.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer:

- Add a dedicated non-root user and `USER` to the final stage. The binary needs no privileged port at runtime; the documented deployments bind 443 inside a network namespace that maps it.
- Pin the builder image by digest, with the version named in a comment so a bump is a one-line diff.
- Leave release signing out of scope. The installer still trusts TLS and a same-origin checksum; changing that needs its own decision about a signing key and who holds it.

Tests: the image builds and the entrypoint runs as the unprivileged user.

Public API: none. Wire: none.

## Related

- [Release execution](/quest/m2/release/README.md) - the image publish path this changes
