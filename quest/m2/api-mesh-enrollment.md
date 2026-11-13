# [XS] A mesh enrollment reply can grow

## Goal

`moq_auth::mesh::Enrollment` and the other reply and request structs the mesh
module exports are `#[non_exhaustive]` with constructors where a caller builds
one, so a later field (a certificate, a revocation hint) is additive. The cut
publishes the module for the first time, and today `Enrollment` is a plain
struct with a public `token: String`.

## Plan

Decided 2026-11-12 (maintainer, /quest-audit before the cut):

- ✅ Fence the structs now (rejected: publishing them open and breaking
  later).
- ✅ The mesh [relay node](/quest/m3/mesh/node.md) takes a credential handle
  and the application enrolls however it likes (rejected: the coordinator and
  `Enrollment` learning to issue certificates; deferring). So this quest adds
  no certificate field; it only keeps one possible.

Sweep `rs/moq-auth/src/mesh/` for every public struct and enum a caller
matches or builds, and fence each the same way. Public API: breaking for a
caller that builds or exhaustively destructures one. Wire: none.
