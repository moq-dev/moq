# [L] Relay auth

## Goal

Every relay authorization source enforces versioned pattern grants through
pattern-scoped origin handles: JWTs, static config, anonymous public access,
the auth API, and live revalidation.

## Plan

Land reader-first: readers accept v1 before any writer emits it.

- Version the relay's public/static/auth-API grant shapes. Unversioned
  `publish`/`subscribe` arrays remain v0 prefixes; new writers emit v1.
- Flow verified token and public grants into pattern-scoped producers and
  consumers. A literal root aliases or rebases the patterns but never becomes
  one.
- Revalidation compares the full versioned grant and resizes a live session
  without a prefix-only widening window.
- Keep auth failures explicit: unsupported versions, mixed fields, invalid
  patterns, and scope escapes reject the connection or refresh.
- Cover JWT, public, static, alias, auth-API, revalidation, HLS, and cluster
  paths with inside/outside pattern tests and v0 compatibility fixtures.

## Required

- [Origin scopes](/quest/m2/path-patterns/origin.md)
- [Claims](/quest/m2/path-patterns/claims.md)
