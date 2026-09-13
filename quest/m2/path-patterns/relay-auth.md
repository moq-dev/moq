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
- Own one resize operation that cancels active publications and subscriptions
  outside the new grant while preserving still-authorized work and the
  connection, even when the resulting grant is empty. Prove this behavior
  through revalidation here. [Relay tokens](/quest/m2/auth/relay-refresh.md)
  later feeds token unions into this operation and owns AUTH expiry tests;
  this quest does not wait for in-band tokens.
- Keep auth failures explicit: unsupported versions, mixed fields, invalid
  patterns, and scope escapes reject the connection or refresh.
- Cover JWT, public, static, alias, auth-API, revalidation, HLS, and cluster
  paths with inside/outside pattern tests and v0 compatibility fixtures.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - the required M1 APIs must be available on main before this implementation starts

- [Origin scopes](/quest/m2/path-patterns/origin.md)
- [Claims](/quest/m1/api-token-claims.md)
