# [XS] The relay image runs unprivileged and pins its builder

## Goal

The published container image runs as a non-root user, and its builder image is pinned by digest, so the build tooling cannot drift under the same source.

Today the final stage is `FROM scratch` with no `USER`, so the relay runs as uid 0, and the builder is `docker.io/nixos/nix:latest`.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer:

- Add a dedicated non-root user and `USER` to the final stage.
- `rs/moq-relay/fly.toml` binds 443 directly (`MOQ_LISTEN` and `internal_port`), which an unprivileged process cannot. Prefer moving that deployment to an unprivileged internal port behind the same public 443, if Fly's UDP forwarding allows it; otherwise grant `CAP_NET_BIND_SERVICE` explicitly.
- Pin the builder image by digest, with the version named in a comment so a bump is a one-line diff. The `Dockerfile` also names the same image as the source of the `/nix` cache mount; both references use the one digest, ideally through a shared pinned stage.
- Leave release signing out of scope. The installer still trusts TLS and a same-origin checksum; changing that needs its own decision about a signing key and who holds it.

Tests: the image builds, the entrypoint runs as the unprivileged user, and the Fly listen configuration starts under it.

Public API: none. Wire: none.

## Related

- [Release execution](/quest/m2/release/README.md) - the image publish path this changes
