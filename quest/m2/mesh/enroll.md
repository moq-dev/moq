# [M] Enrollment

## Goal

A device with an endpoint key joins a mesh and receives the relay URLs to dial
and a token granting publish on `mesh/<id>/**` and subscribe on `mesh/**`,
refreshed before it expires. A human enrolls a device through the OAuth device
grant (RFC 8628); a fleet of robots enrolls headlessly with a pre-authorized
enrollment key. `moq auth` serves the contract for self-hosters.

## Plan

See the [epic](/quest/m2/mesh/README.md) for the decisions.

- Enrollment proves possession of the endpoint key against a fresh server
  challenge, so a token is never issued for a key the caller does not hold.
- Enrollment keys are created by the mesh owner, may carry tags and an expiry,
  and are single-use or reusable; a revoked key enrolls nothing new.
- Revoking a device makes its next refresh fail; the relay refuses the
  expired token as it does today.
- A device has a name (defaulting to its hostname) used by
  [MagicDNS](/quest/m2/mesh/dns.md); the contract returns every member's name
  through the announcement metadata or a mesh-scoped track, settled here.
- Write the contract as a doc page section in the [guide](/quest/m2/mesh/guide.md)
  so a hosted service can implement it. Report the public API and any token
  claim it adds.

Tests: device grant approve and deny, a reused enrollment key refused when
single-use, a challenge signed by the wrong key refused, refresh, and
revocation.
